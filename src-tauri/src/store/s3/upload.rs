use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use aws_smithy_types::byte_stream::{ByteStream, Length};
use futures::stream::StreamExt;

use super::util::{canceled, s3_err, ProgressThrottle};
use super::{classify_aws, S3Store};
use crate::error::{AppError, AppResult};
use crate::store::{CannedAcl, ObjectStore, PutOptions, Sse};
use crate::transfer::source_guard::open_checked;
use crate::transfer::{CompletedPart as SavedPart, ResumeState, SourceGuard, TransferCtx, TransferEvent, UploadResult};

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
        // If-None-Match is honored natively, or emulated with HEAD; never silently dropped.
        let mut cond = opts.if_none_match.is_some();
        if cond && self.inm_unsupported.load(Ordering::Relaxed) {
            crate::store::ensure_absent(self, bucket, key).await?;
            cond = false;
        }
        let etag = match self.send_put(bucket, key, &source, &opts, cond, &ctx).await {
            Err(AppError::Unsupported(_)) if cond => {
                self.inm_unsupported.store(true, Ordering::Relaxed);
                crate::store::ensure_absent(self, bucket, key).await?;
                self.send_put(bucket, key, &source, &opts, false, &ctx).await?
            }
            r => r?,
        };

        ctx.progress.emit(TransferEvent::Progress {
            transfer_id: ctx.transfer_id.clone(),
            bytes_done: total,
            bytes_total: Some(total),
        });
        ctx.progress.emit(TransferEvent::Done {
            transfer_id: ctx.transfer_id.clone(),
            etag: etag.clone(),
        });

        Ok(UploadResult { etag, upload_id: None })
    }

    async fn send_put(
        &self,
        bucket: &str,
        key: &str,
        source: &Path,
        opts: &PutOptions,
        if_none_match: bool,
        ctx: &TransferCtx,
    ) -> AppResult<Option<String>> {
        let body = source_body(source, opts.source_guard.as_ref(), None).await?;
        let mut req = self
            .client
            .put_object()
            .bucket(bucket)
            .key(key)
            .body(body)
            .set_content_type(opts.content_type.clone())
            .set_cache_control(opts.cache_control.clone())
            .set_content_disposition(opts.content_disposition.clone())
            .set_content_encoding(opts.content_encoding.clone())
            .set_if_match(opts.if_match.clone());
        for (k, v) in &opts.user_metadata {
            req = req.metadata(k, v);
        }
        if if_none_match {
            req = req.set_if_none_match(opts.if_none_match.clone());
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

        let resp = tokio::select! {
            _ = ctx.cancel.cancelled() => {
                ctx.progress.emit(TransferEvent::Canceled { transfer_id: ctx.transfer_id.clone() });
                return Err(canceled(&ctx.transfer_id));
            }
            r = req.send() => r.map_err(|e| classify_aws("put_object", e))?,
        };
        Ok(resp.e_tag().map(|s| s.to_string()))
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
            self.init_or_resume_upload(bucket, key, total, &opts, &ctx).await?;
        // Durable before any part is sent, so a crash can never orphan the upload.
        if let Some(h) = &ctx.on_upload_id {
            h.call(&upload_id, part_size).await;
        }
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
        let guard_s = opts.source_guard.clone();
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
            let guard = guard_s.clone();
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

                let body = source_body(&source, guard.as_ref(), Some((offset, length))).await?;

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
        let cond = opts.if_none_match.is_some();
        let result = if cond && self.inm_unsupported.load(Ordering::Relaxed) {
            crate::store::ensure_absent(self, bucket, key).await?;
            self.complete_multipart(bucket, key, &upload_id, &collected, false).await?
        } else {
            match self.complete_multipart(bucket, key, &upload_id, &collected, cond).await {
                // Rejected header leaves the upload open: check by HEAD, then complete plainly.
                Err(AppError::Unsupported(_)) if cond => {
                    self.inm_unsupported.store(true, Ordering::Relaxed);
                    crate::store::ensure_absent(self, bucket, key).await?;
                    self.complete_multipart(bucket, key, &upload_id, &collected, false).await?
                }
                r => r?,
            }
        };

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
        total: u64,
        opts: &PutOptions,
        ctx: &TransferCtx,
    ) -> AppResult<(String, Vec<SavedPart>, u64)> {
        let fresh_part_size = adaptive_part_size(ctx.part_size, total);
        if let Some(state) = ctx.resume.clone() {
            // A saved layout past S3 limits can never complete: restart with the adaptive size.
            let layout_ok = state.part_size.is_some_and(|p| p <= MAX_PART && total.div_ceil(p.max(1)) <= MAX_PARTS);
            if !state.upload_id.is_empty() && layout_ok && !resume_state_is_stale(&state, ctx) {
                let part_size = state.part_size.unwrap_or(fresh_part_size);
                // The journal is sparse (power-of-two flushes); the provider's part list is not.
                match self.list_all_parts(bucket, key, &state.upload_id).await {
                    Ok(listed) => {
                        return Ok((state.upload_id, usable_parts(listed, total, part_size), part_size));
                    }
                    // No ListParts (unimplemented or not permitted): trust the journal.
                    Err(AppError::Unsupported(_) | AppError::AccessDenied(_)) => {
                        return Ok((state.upload_id, state.completed_parts, part_size));
                    }
                    // An aborted/expired upload fails every part with NoSuchUpload: start fresh.
                    Err(e) if e.is_not_found() => {}
                    Err(e) => return Err(e),
                }
            } else if !state.upload_id.is_empty() {
                // Offline: keep the old upload_id (and its row) rather than orphan it.
                match self.abort_multipart_upload(bucket, key, &state.upload_id).await {
                    Err(e) if e.is_network() => return Err(e),
                    Err(e) if !e.is_not_found() => tracing::warn!("abort stale multipart upload failed: {e}"),
                    _ => {}
                }
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

    /// Every uploaded part of `upload_id` as (part, size), following ListParts pagination.
    async fn list_all_parts(&self, bucket: &str, key: &str, upload_id: &str) -> AppResult<Vec<(SavedPart, Option<i64>)>> {
        let mut out = Vec::new();
        let mut marker: Option<String> = None;
        loop {
            let mut req = self.client.list_parts().bucket(bucket).key(key).upload_id(upload_id);
            if let Some(m) = &marker {
                req = req.part_number_marker(m);
            }
            let resp = req.send().await.map_err(|e| classify_aws("list_parts", e))?;
            for p in resp.parts() {
                if let (Some(n), Some(etag)) = (p.part_number(), p.e_tag()) {
                    out.push((SavedPart { part_number: n, etag: etag.to_string() }, p.size()));
                }
            }
            let next = resp.next_part_number_marker().filter(|m| !m.is_empty()).map(str::to_string);
            // A marker that does not advance would loop forever.
            if resp.is_truncated().unwrap_or(false) && next.is_some() && next != marker {
                marker = next;
            } else {
                break;
            }
        }
        Ok(out)
    }

    async fn complete_multipart(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        parts: &[SavedPart],
        if_none_match: bool,
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
            .set_if_none_match(if_none_match.then(|| "*".to_string()))
            .send()
            .await
            .map_err(|e| classify_aws("complete_multipart_upload", e))?;

        Ok(UploadResult {
            etag: resp.e_tag().map(|s| s.to_string()),
            upload_id: Some(upload_id.to_string()),
        })
    }
}

const MIB: u64 = 1024 * 1024;
const MAX_PARTS: u64 = 10_000;
/// S3 part size ceiling.
const MAX_PART: u64 = 5 * 1024 * MIB;

/// Body from the source file, through the guard's checked open when set.
async fn source_body(source: &Path, guard: Option<&SourceGuard>, part: Option<(u64, u64)>) -> AppResult<ByteStream> {
    let mut b = ByteStream::read_from();
    b = match guard {
        Some(g) => b.file(open_checked(source, g).await?),
        None => b.path(source),
    };
    if let Some((offset, len)) = part {
        b = b.offset(offset).length(Length::Exact(len));
    }
    b.build().await.map_err(|e| s3_err("bytestream", e))
}

/// Configured size (5 MiB floor, 5 GiB cap) when it fits in 10,000 parts, else the smallest whole-MiB size that does.
pub(crate) fn adaptive_part_size(configured: u64, total: u64) -> u64 {
    let base = configured.clamp(5 * MIB, MAX_PART);
    if total.div_ceil(base) <= MAX_PARTS {
        return base;
    }
    total.div_ceil(MAX_PARTS).div_ceil(MIB) * MIB
}

/// Listed parts that fit the current layout; a wrong-sized part is dropped and re-sent.
fn usable_parts(listed: Vec<(SavedPart, Option<i64>)>, total: u64, part_size: u64) -> Vec<SavedPart> {
    let num_parts = total.div_ceil(part_size) as i64;
    listed
        .into_iter()
        .filter(|(p, size)| {
            let n = p.part_number as i64;
            if n < 1 || n > num_parts {
                return false;
            }
            let expected = total.saturating_sub((n as u64 - 1) * part_size).min(part_size) as i64;
            size.map_or(true, |s| s == expected)
        })
        .map(|(p, _)| p)
        .collect()
}

/// Stale unless saved parts provably match the current source (len+mtime) with a known part size; mixing versions corrupts the object.
fn resume_state_is_stale(state: &ResumeState, ctx: &TransferCtx) -> bool {
    match (state.source_len, state.source_mtime_secs, ctx.source_stat, state.part_size) {
        (Some(len), Some(mtime), Some(cur), Some(_)) => len != cur.len || mtime != cur.mtime_secs,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_size_keeps_configured_when_it_fits() {
        assert_eq!(adaptive_part_size(8 * MIB, 20 * MIB), 8 * MIB);
        // Exactly 10,000 parts still fits.
        assert_eq!(adaptive_part_size(8 * MIB, 10_000 * 8 * MIB), 8 * MIB);
        // Below the S3 floor the floor wins, as before.
        assert_eq!(adaptive_part_size(MIB, 100 * MIB), 5 * MIB);
        // A huge configured size is capped at the S3 part maximum.
        assert_eq!(adaptive_part_size(8 * 1024 * MIB, 20 * 1024 * MIB), MAX_PART);
    }

    #[test]
    fn part_size_grows_past_ten_thousand_parts() {
        let total = 10_000 * 8 * MIB + 1;
        let p = adaptive_part_size(8 * MIB, total);
        assert_eq!(p, 9 * MIB);
        assert!(total.div_ceil(p) <= MAX_PARTS);
        // 5 TiB, the S3 object maximum.
        let five_tib = 5 * 1024 * 1024 * MIB;
        let p = adaptive_part_size(8 * MIB, five_tib);
        assert_eq!(p % MIB, 0);
        assert!(five_tib.div_ceil(p) <= MAX_PARTS);
        assert!(five_tib.div_ceil(p - MIB) > MAX_PARTS, "smallest whole-MiB size that fits");
    }

    #[test]
    fn usable_parts_drops_out_of_layout_parts() {
        let part = |n: i32| SavedPart { part_number: n, etag: format!("e{n}") };
        let total = 12 * MIB;
        let listed = vec![
            (part(1), Some((5 * MIB) as i64)),
            (part(2), Some(MIB as i64)),
            (part(3), Some((2 * MIB) as i64)),
            (part(4), Some((2 * MIB) as i64)),
        ];
        let got: Vec<i32> = usable_parts(listed, total, 5 * MIB).iter().map(|p| p.part_number).collect();
        assert_eq!(got, vec![1, 3], "wrong-size part 2 and out-of-range part 4 are re-sent");
    }
}
