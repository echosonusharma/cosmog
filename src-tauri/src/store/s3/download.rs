use std::path::PathBuf;
use std::sync::Arc;

use aws_smithy_runtime_api::client::result::SdkError;
use aws_smithy_types::error::metadata::ProvideErrorMetadata;
use futures::stream::StreamExt;
use tokio::fs::File;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use super::util::{canceled, s3_err, ProgressThrottle};
use super::{classify_aws, S3Store};
use crate::error::{AppError, AppResult};
use crate::store::{GetOptions, ObjectPreview};
use crate::transfer::{DownloadResult, TransferCtx, TransferEvent};

impl S3Store {
    pub(super) async fn read_range(
        &self,
        bucket: &str,
        key: &str,
        max_bytes: u64,
    ) -> AppResult<ObjectPreview> {
        // Sized for the largest in-app viewer (PDF, 25 MB); callers pass tighter caps.
        const HARD_CAP: u64 = 32 * 1024 * 1024;
        let cap = max_bytes.min(HARD_CAP);

        // Full-object fallback ONLY on 416/InvalidRange; masking other errors caused confusing full downloads.
        let range_str = format!("bytes=0-{}", cap.saturating_sub(1));
        let ranged = self
            .client
            .get_object()
            .bucket(bucket)
            .key(key)
            .range(&range_str)
            .send()
            .await;

        let (resp, used_range) = match ranged {
            Ok(r) => (r, true),
            Err(e) if range_request_rejected(&e) => {
                let r = self
                    .client
                    .get_object()
                    .bucket(bucket)
                    .key(key)
                    .send()
                    .await
                    .map_err(|e| classify_aws("get_object", e))?;
                (r, false)
            }
            Err(e) => return Err(classify_aws("get_object preview range", e)),
        };

        // Ranged GET: content_length is the range size; Content-Range has the real total.
        let total_from_range = if used_range {
            resp.content_range().and_then(parse_content_range_total)
        } else {
            None
        };
        let payload_len = resp.content_length();
        if let Some(len) = payload_len {
            if len < 0 {
                return Err(AppError::S3(format!(
                    "get_object preview: negative content length {len}"
                )));
            }
        }
        let content_type = resp.content_type().map(|s| s.to_string());
        let mut body = resp.body;
        let mut bytes: Vec<u8> = Vec::with_capacity(
            payload_len
                .unwrap_or(0)
                .clamp(0, cap as i64) as usize,
        );
        // Hard RAM cap: buffer at most `cap` bytes even if Content-Length lies.
        while bytes.len() < cap as usize {
            let next = body
                .try_next()
                .await
                .map_err(|e| s3_err("get_object preview body", e))?;
            match next {
                Some(chunk) => {
                    let room = cap as usize - bytes.len();
                    if chunk.len() > room {
                        bytes.extend_from_slice(&chunk[..room]);
                        break;
                    }
                    bytes.extend_from_slice(&chunk);
                }
                None => break,
            }
        }
        drop(body);
        let total = total_from_range.or(payload_len);
        let truncated = total
            .map(|t| (t as u64) > bytes.len() as u64)
            .unwrap_or(false);
        Ok(ObjectPreview {
            bytes,
            content_type,
            total_size: total,
            truncated,
        })
    }

    pub(super) async fn download(
        &self,
        bucket: &str,
        key: &str,
        dest: PathBuf,
        opts: GetOptions,
        ctx: TransferCtx,
    ) -> AppResult<DownloadResult> {
        if ctx.cancel.is_cancelled() {
            ctx.progress.emit(TransferEvent::Canceled {
                transfer_id: ctx.transfer_id.clone(),
            });
            return Err(canceled(&ctx.transfer_id));
        }
        if opts.range_start.is_none() && opts.range_end.is_none() {
            // HEAD the requested version, else an older version gets the latest's size.
            let head_req = self
                .client
                .head_object()
                .bucket(bucket)
                .key(key)
                .set_version_id(opts.version_id.clone());
            if let Ok(head) = head_req.send().await {
                if let Some(total) = head.content_length().map(|n| n as u64) {
                    if total > ctx.multipart_threshold {
                        let meta = head.metadata().cloned().unwrap_or_default();
                        return self
                            .get_object_parallel(bucket, key, &dest, total, &opts, &ctx, meta)
                            .await;
                    }
                }
            }
        }

        let mut opts = opts;
        let resp = loop {
            let mut req = self
                .client
                .get_object()
                .bucket(bucket)
                .key(key)
                .set_version_id(opts.version_id.clone());
            if let Some(range) = build_range_header(opts.range_start, opts.range_end) {
                req = req.range(range);
            }
            if let (true, Some(secs)) = (opts.resume, opts.resume_unmodified_since) {
                req = req.if_unmodified_since(aws_smithy_types::DateTime::from_secs(secs));
            }
            match req.send().await {
                Ok(r) => break r,
                // Offset past the end, or object replaced since the partial: restart.
                Err(e) if opts.resume && (range_request_rejected(&e) || precondition_failed(&e)) => {
                    opts.resume = false;
                    opts.range_start = None;
                }
                Err(e) => return Err(classify_aws("get_object", e)),
            }
        };
        let user_metadata = resp.metadata().cloned().unwrap_or_default();

        // Ranged GET: content_length is range-size; Content-Range carries the real total.
        let total = resp
            .content_range()
            .and_then(parse_content_range_total)
            .map(|n| n as u64)
            .or_else(|| resp.content_length().map(|n| n as u64));
        ctx.progress.emit(TransferEvent::Started {
            transfer_id: ctx.transfer_id.clone(),
            bytes_total: total,
        });

        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        // Append only on an explicit resume; inferring from dest.exists() appended onto unrelated files.
        let resume_offset = if opts.resume && dest.exists() {
            opts.range_start.unwrap_or(0)
        } else {
            0
        };
        let mut file = if opts.resume && resume_offset > 0 && dest.exists() {
            tokio::fs::OpenOptions::new()
                .write(true)
                .append(true)
                .open(&dest)
                .await?
        } else {
            File::create(&dest).await?
        };

        let throttle = ProgressThrottle::new(200);
        let mut bytes_done: u64 = resume_offset;
        let mut stream = resp.body;

        loop {
            tokio::select! {
                _ = ctx.cancel.cancelled() => {
                    drop(file);
                    // Delete only what we created; a pre-existing partial stays for retry resume.
                    if !(opts.resume && resume_offset > 0) {
                        let _ = tokio::fs::remove_file(&dest).await;
                    }
                    ctx.progress.emit(TransferEvent::Canceled { transfer_id: ctx.transfer_id.clone() });
                    return Err(canceled(&ctx.transfer_id));
                }
                next = stream.try_next() => {
                    match next.map_err(|e| s3_err("get_object stream", e))? {
                        Some(chunk) => {
                            file.write_all(&chunk).await?;
                            bytes_done += chunk.len() as u64;
                            if throttle.allow() {
                                ctx.progress.emit(TransferEvent::Progress {
                                    transfer_id: ctx.transfer_id.clone(),
                                    bytes_done,
                                    bytes_total: total,
                                });
                            }
                        }
                        None => break,
                    }
                }
            }
        }

        file.flush().await?;
        ctx.progress.emit(TransferEvent::Progress {
            transfer_id: ctx.transfer_id.clone(),
            bytes_done,
            bytes_total: total,
        });
        ctx.progress.emit(TransferEvent::Done {
            transfer_id: ctx.transfer_id.clone(),
            etag: None,
        });

        Ok(DownloadResult { bytes: bytes_done, user_metadata })
    }

    async fn get_object_parallel(
        &self,
        bucket: &str,
        key: &str,
        dest: &PathBuf,
        total: u64,
        opts: &GetOptions,
        ctx: &TransferCtx,
        user_metadata: std::collections::HashMap<String, String>,
    ) -> AppResult<DownloadResult> {
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let sparse = sparse_path(dest);
        {
            let file = File::create(&sparse).await?;
            file.set_len(total).await?;
        }

        let part_size = ctx.part_size.max(5 * 1024 * 1024);
        let num_parts = (total + part_size - 1) / part_size;

        let client = self.client.clone();
        let bucket_s = bucket.to_string();
        let key_s = key.to_string();
        let version_id = opts.version_id.clone();
        let dest_s = sparse.clone();
        let progress = ctx.progress.clone();
        let cancel = ctx.cancel.clone();
        let transfer_id = ctx.transfer_id.clone();
        let throttle = Arc::new(ProgressThrottle::new(200));
        let bytes_done_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));

        ctx.progress.emit(TransferEvent::Started {
            transfer_id: transfer_id.clone(),
            bytes_total: Some(total),
        });

        let futures_iter = (0..num_parts).map(|part_no| {
            let client = client.clone();
            let bucket = bucket_s.clone();
            let key = key_s.clone();
            let version_id = version_id.clone();
            let dest = dest_s.clone();
            let progress = progress.clone();
            let cancel = cancel.clone();
            let transfer_id = transfer_id.clone();
            let throttle = throttle.clone();
            let bytes_done_counter = bytes_done_counter.clone();

            async move {
                if cancel.is_cancelled() {
                    return Err(AppError::Canceled(format!("transfer {transfer_id} canceled")));
                }
                let offset = part_no * part_size;
                let length = (total - offset).min(part_size);
                let range = format!("bytes={}-{}", offset, offset + length - 1);

                let mut req = client.get_object().bucket(&bucket).key(&key).range(range);
                if let Some(vid) = &version_id {
                    req = req.version_id(vid);
                }

                let resp = tokio::select! {
                    _ = cancel.cancelled() => return Err(AppError::Canceled(format!("transfer {transfer_id} canceled"))),
                    r = req.send() => r.map_err(|e| classify_aws("get_object_parallel", e))?,
                };

                // Write chunk-by-chunk so resident memory stays one chunk (~256 KiB), not the whole part.
                let mut f = tokio::fs::OpenOptions::new()
                    .write(true)
                    .open(&dest)
                    .await?;
                f.seek(std::io::SeekFrom::Start(offset)).await?;
                let mut stream = resp.body;
                loop {
                    let next = tokio::select! {
                        _ = cancel.cancelled() => return Err(AppError::Canceled(format!("transfer {transfer_id} canceled"))),
                        n = stream.try_next() => n.map_err(|e| s3_err("get_object_parallel body", e))?,
                    };
                    match next {
                        Some(chunk) => {
                            f.write_all(&chunk).await?;
                            let done = bytes_done_counter
                                .fetch_add(chunk.len() as u64, std::sync::atomic::Ordering::Relaxed)
                                + chunk.len() as u64;
                            if throttle.allow() {
                                progress.emit(TransferEvent::Progress {
                                    transfer_id: transfer_id.clone(),
                                    bytes_done: done,
                                    bytes_total: Some(total),
                                });
                            }
                        }
                        None => break,
                    }
                }
                f.flush().await?;

                Ok::<_, AppError>(())
            }
        });

        let mut stream = Box::pin(
            futures::stream::iter(futures_iter).buffer_unordered(ctx.parallelism.max(1))
        );
        while let Some(res) = stream.next().await {
            if let Err(e) = res {
                drop(stream);
                // No terminal event: the worker may retry, and it emits the single terminal.
                let _ = tokio::fs::remove_file(&sparse).await;
                return Err(e);
            }
        }
        tokio::fs::rename(&sparse, dest).await?;

        ctx.progress.emit(TransferEvent::Progress {
            transfer_id: ctx.transfer_id.clone(),
            bytes_done: total,
            bytes_total: Some(total),
        });
        ctx.progress.emit(TransferEvent::Done {
            transfer_id: ctx.transfer_id.clone(),
            etag: None,
        });

        Ok(DownloadResult { bytes: total, user_metadata })
    }
}

/// Parallel downloads preallocate (holes), so they write here and rename onto `dest`.
fn sparse_path(dest: &std::path::Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".sparse");
    dest.with_file_name(name)
}

/// 416/InvalidRange: the only failure where a full-object fallback is meaningful.
fn range_request_rejected<E>(
    err: &SdkError<E, aws_smithy_runtime_api::client::orchestrator::HttpResponse>,
) -> bool
where
    E: ProvideErrorMetadata + std::fmt::Display + std::fmt::Debug,
{
    if let Some(service_err) = err.as_service_error() {
        let code = service_err.code().unwrap_or_default();
        if matches!(code, "InvalidRange" | "RangeNotSatisfiable") {
            return true;
        }
    }
    if let SdkError::ServiceError(se) = err {
        if se.raw().status().as_u16() == 416 {
            return true;
        }
    }
    false
}

fn precondition_failed<E>(
    err: &SdkError<E, aws_smithy_runtime_api::client::orchestrator::HttpResponse>,
) -> bool {
    matches!(err, SdkError::ServiceError(se) if se.raw().status().as_u16() == 412)
}

/// Total from `Content-Range: bytes a-b/total`; `None` for `*` or malformed input.
fn parse_content_range_total(header: &str) -> Option<i64> {
    let after_slash = header.rsplit_once('/')?.1.trim();
    if after_slash == "*" {
        return None;
    }
    after_slash.parse::<i64>().ok()
}

fn build_range_header(start: Option<u64>, end: Option<u64>) -> Option<String> {
    match (start, end) {
        (None, None) => None,
        (Some(s), Some(e)) => Some(format!("bytes={s}-{e}")),
        (Some(s), None) => Some(format!("bytes={s}-")),
        (None, Some(e)) => Some(format!("bytes=0-{e}")),
    }
}
