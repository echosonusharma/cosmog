use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use aws_smithy_types::byte_stream::{ByteStream, Length};
use futures::stream::StreamExt;

use super::util::{canceled, s3_err, ProgressThrottle};
use super::{classify_aws, S3Store};
use crate::error::{AppError, AppResult};
use crate::store::{CannedAcl, ObjectStore, PutOptions, Sse};
use crate::transfer::{CompletedPart as SavedPart, ResumeState, TransferCtx, TransferEvent, UploadResult};

impl S3Store {
    pub(super) async fn put_single(
        &self,
        bucket: &str,
        key: &str,
        source: PathBuf,
        total: u64,
        opts: PutOptions,
        ctx: TransferCtx,
    ) -> AppResult<UploadResult> {
        if ctx.cancel.is_cancelled() {
            ctx.progress.emit(TransferEvent::Canceled {
                transfer_id: ctx.transfer_id.clone(),
            });
            return Err(canceled(&ctx.transfer_id));
        }

        let body = ByteStream::from_path(&source)
            .await
            .map_err(|e| s3_err("bytestream", e))?;

        let mut req = self
            .client
            .put_object()
            .bucket(bucket)
            .key(key)
            .body(body);

        if let Some(ct) = opts.content_type {
            req = req.content_type(ct);
        }
        if let Some(cc) = opts.cache_control {
            req = req.cache_control(cc);
        }
        if let Some(cd) = opts.content_disposition {
            req = req.content_disposition(cd);
        }
        if let Some(ce) = opts.content_encoding {
            req = req.content_encoding(ce);
        }
        for (k, v) in opts.user_metadata {
            req = req.metadata(k, v);
        }
        if let Some(im) = opts.if_match {
            req = req.if_match(im);
        }
        if let Some(inm) = opts.if_none_match {
            req = req.if_none_match(inm);
        }
        if let Some(sse) = opts.sse {
            use aws_sdk_s3::types::ServerSideEncryption;
            match sse {
                Sse::S3 => {
                    req = req.server_side_encryption(ServerSideEncryption::Aes256);
                }
                Sse::Kms { key_id } => {
                    req = req.server_side_encryption(ServerSideEncryption::AwsKms);
                    if let Some(k) = key_id {
                        req = req.ssekms_key_id(k);
                    }
                }
            }
        }
        if let Some(acl) = opts.acl {
            use aws_sdk_s3::types::ObjectCannedAcl;
            let v = match acl {
                CannedAcl::Private => ObjectCannedAcl::Private,
                CannedAcl::PublicRead => ObjectCannedAcl::PublicRead,
            };
            req = req.acl(v);
        }

        let resp = tokio::select! {
            _ = ctx.cancel.cancelled() => {
                ctx.progress.emit(TransferEvent::Canceled { transfer_id: ctx.transfer_id.clone() });
                return Err(canceled(&ctx.transfer_id));
            }
            r = req.send() => r.map_err(|e| classify_aws("put_object", e))?,
        };

        ctx.progress.emit(TransferEvent::Progress {
            transfer_id: ctx.transfer_id.clone(),
            bytes_done: total,
            bytes_total: Some(total),
        });
        ctx.progress.emit(TransferEvent::Done {
            transfer_id: ctx.transfer_id.clone(),
            etag: resp.e_tag().map(|s| s.to_string()),
        });

        Ok(UploadResult {
            etag: resp.e_tag().map(|s| s.to_string()),
            upload_id: None,
        })
    }

    pub(super) async fn put_multipart(
        &self,
        bucket: &str,
        key: &str,
        source: PathBuf,
        total: u64,
        opts: PutOptions,
        ctx: TransferCtx,
    ) -> AppResult<UploadResult> {
        let (upload_id, already_done, part_size) =
            self.init_or_resume_upload(bucket, key, &opts, &ctx).await?;
        // Publish upload_id now so retries resume it even with zero parts done; the worker owns the abort.
        ctx.progress.emit(TransferEvent::MultipartInitiated {
            transfer_id: ctx.transfer_id.clone(),
            upload_id: upload_id.clone(),
            part_size,
        });

        let num_parts_u64 = (total + part_size - 1) / part_size;
        if num_parts_u64 > 10_000 {
            return Err(crate::error::AppError::InvalidInput(format!(
                "file too large: would require {num_parts_u64} parts (S3 limit is 10,000); increase part size"
            )));
        }
        let num_parts = num_parts_u64 as i32;
        let bytes_done = Arc::new(AtomicU64::new(
            // Sum completed parts' actual lengths (final part is shorter).
            already_done
                .iter()
                .map(|p| {
                    let off = (p.part_number - 1) as u64 * part_size;
                    let remaining = total.saturating_sub(off);
                    remaining.min(part_size)
                })
                .sum::<u64>()
                .min(total),
        ));
        let done_set: std::collections::HashSet<i32> =
            already_done.iter().map(|p| p.part_number).collect();

        let throttle = Arc::new(ProgressThrottle::new(200));

        let to_upload: Vec<i32> = (1..=num_parts).filter(|p| !done_set.contains(p)).collect();

        let client = self.client.clone();
        let bucket_s = bucket.to_string();
        let key_s = key.to_string();
        let upload_id_s = upload_id.clone();
        let source_s = source.clone();
        let progress = ctx.progress.clone();
        let cancel = ctx.cancel.clone();
        let transfer_id = ctx.transfer_id.clone();
        let bytes_done_outer = bytes_done.clone();
        let throttle_outer = throttle.clone();

        let upload_futures = futures::stream::iter(to_upload.into_iter().map(|part_no| {
            let client = client.clone();
            let bucket = bucket_s.clone();
            let key = key_s.clone();
            let upload_id = upload_id_s.clone();
            let source = source_s.clone();
            let progress = progress.clone();
            let cancel = cancel.clone();
            let transfer_id = transfer_id.clone();
            let bytes_done = bytes_done_outer.clone();
            let throttle = throttle_outer.clone();

            async move {
                if cancel.is_cancelled() {
                    return Err(canceled(&transfer_id));
                }
                let offset = (part_no - 1) as u64 * part_size;
                let remaining = total - offset;
                let length = remaining.min(part_size);

                let body = ByteStream::read_from()
                    .path(&source)
                    .offset(offset)
                    .length(Length::Exact(length))
                    .build()
                    .await
                    .map_err(|e| s3_err("bytestream part", e))?;

                let send_fut = client
                    .upload_part()
                    .bucket(&bucket)
                    .key(&key)
                    .upload_id(&upload_id)
                    .part_number(part_no)
                    .body(body)
                    .send();

                let resp = tokio::select! {
                    _ = cancel.cancelled() => return Err(canceled(&transfer_id)),
                    r = send_fut => r.map_err(|e| classify_aws("upload_part", e))?,
                };

                let etag = resp
                    .e_tag()
                    .ok_or_else(|| AppError::S3("upload_part: no etag".into()))?
                    .to_string();

                let total_done = bytes_done.fetch_add(length, Ordering::Relaxed) + length;
                if throttle.allow() {
                    progress.emit(TransferEvent::Progress {
                        transfer_id: transfer_id.clone(),
                        bytes_done: total_done,
                        bytes_total: Some(total),
                    });
                }
                progress.emit(TransferEvent::PartCompleted {
                    transfer_id: transfer_id.clone(),
                    upload_id: upload_id.clone(),
                    part_number: part_no,
                    etag: etag.clone(),
                });

                Ok::<_, AppError>(SavedPart {
                    part_number: part_no,
                    etag,
                })
            }
        }))
        .buffer_unordered(ctx.parallelism.max(1));

        let mut collected: Vec<SavedPart> = already_done.clone();
        let mut stream = Box::pin(upload_futures);
        while let Some(res) = stream.next().await {
            match res {
                Ok(p) => collected.push(p),
                Err(e) => {
                    // Do NOT abort or emit terminal here: the worker retries on this upload_id and aborts once on give-up.
                    return Err(e);
                }
            }
        }

        collected.sort_by_key(|p| p.part_number);
        let result = self
            .complete_multipart(bucket, key, &upload_id, &collected)
            .await?;

        ctx.progress.emit(TransferEvent::Done {
            transfer_id: ctx.transfer_id.clone(),
            etag: result.etag.clone(),
        });

        Ok(UploadResult {
            etag: result.etag,
            upload_id: Some(upload_id),
        })
    }

    /// Discards saved state (aborting the old upload) when the source changed; mixed versions corrupt the object.
    async fn init_or_resume_upload(
        &self,
        bucket: &str,
        key: &str,
        opts: &PutOptions,
        ctx: &TransferCtx,
    ) -> AppResult<(String, Vec<SavedPart>, u64)> {
        let fresh_part_size = ctx.part_size.max(5 * 1024 * 1024); // S3 floor for non-final parts
        if let Some(state) = ctx.resume.clone() {
            if !state.upload_id.is_empty() && !resume_state_is_stale(&state, ctx) {
                let part_size = state.part_size.unwrap_or(fresh_part_size);
                let probe = self
                    .client
                    .list_parts()
                    .bucket(bucket)
                    .key(key)
                    .upload_id(&state.upload_id)
                    .max_parts(1)
                    .send()
                    .await;
                // An aborted/expired upload fails every part with NoSuchUpload: start fresh.
                match probe.map_err(|e| classify_aws("list_parts", e)) {
                    Ok(_) | Err(AppError::Unsupported(_)) => {
                        return Ok((state.upload_id, state.completed_parts, part_size));
                    }
                    Err(e) if e.is_not_found() => {}
                    Err(e) => return Err(e),
                }
            } else if !state.upload_id.is_empty() {
                // Best-effort abort so incomplete-multipart storage doesn't leak.
                let _ = self
                    .abort_multipart_upload(bucket, key, &state.upload_id)
                    .await;
            }
        }

        let mut req = self.client.create_multipart_upload().bucket(bucket).key(key);
        if let Some(ct) = &opts.content_type {
            req = req.content_type(ct);
        }
        if let Some(cc) = &opts.cache_control {
            req = req.cache_control(cc);
        }
        if let Some(cd) = &opts.content_disposition {
            req = req.content_disposition(cd);
        }
        if let Some(ce) = &opts.content_encoding {
            req = req.content_encoding(ce);
        }
        for (k, v) in &opts.user_metadata {
            req = req.metadata(k, v);
        }
        if let Some(sse) = &opts.sse {
            use aws_sdk_s3::types::ServerSideEncryption;
            match sse {
                Sse::S3 => {
                    req = req.server_side_encryption(ServerSideEncryption::Aes256);
                }
                Sse::Kms { key_id } => {
                    req = req.server_side_encryption(ServerSideEncryption::AwsKms);
                    if let Some(k) = key_id {
                        req = req.ssekms_key_id(k);
                    }
                }
            }
        }
        if let Some(acl) = &opts.acl {
            use aws_sdk_s3::types::ObjectCannedAcl;
            let v = match acl {
                CannedAcl::Private => ObjectCannedAcl::Private,
                CannedAcl::PublicRead => ObjectCannedAcl::PublicRead,
            };
            req = req.acl(v);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| classify_aws("create_multipart_upload", e))?;
        let id = resp
            .upload_id()
            .ok_or_else(|| AppError::S3("multipart: no upload id".into()))?
            .to_string();
        Ok((id, Vec::new(), fresh_part_size))
    }

    async fn complete_multipart(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        parts: &[SavedPart],
    ) -> AppResult<UploadResult> {
        let completed = CompletedMultipartUpload::builder()
            .set_parts(Some(
                parts
                    .iter()
                    .map(|p| {
                        CompletedPart::builder()
                            .part_number(p.part_number)
                            .e_tag(p.etag.clone())
                            .build()
                    })
                    .collect(),
            ))
            .build();

        let resp = self
            .client
            .complete_multipart_upload()
            .bucket(bucket)
            .key(key)
            .upload_id(upload_id)
            .multipart_upload(completed)
            .send()
            .await
            .map_err(|e| classify_aws("complete_multipart_upload", e))?;

        Ok(UploadResult {
            etag: resp.e_tag().map(|s| s.to_string()),
            upload_id: Some(upload_id.to_string()),
        })
    }
}

/// Stale unless saved parts provably match the current source (len+mtime) with a known part size; mixing versions corrupts the object.
fn resume_state_is_stale(state: &ResumeState, ctx: &TransferCtx) -> bool {
    match (state.source_len, state.source_mtime_secs, ctx.source_stat, state.part_size) {
        (Some(len), Some(mtime), Some(cur), Some(_)) => len != cur.len || mtime != cur.mtime_secs,
        _ => true,
    }
}
