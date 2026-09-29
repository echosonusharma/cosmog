use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use futures::stream::{self, StreamExt, TryStreamExt};

use super::{classify_aws, S3Store};
use crate::error::{AppError, AppResult};

// CopyObject's hard limit.
const SINGLE_COPY_MAX: i64 = 5 * 1024 * 1024 * 1024;
const MIN_PART: i64 = 512 * 1024 * 1024;
const MAX_PARTS: i64 = 10_000;
const PART_CONCURRENCY: usize = 4;

fn copy_source(bucket: &str, key: &str, version_id: Option<&str>) -> String {
    let encoded_key: String = key
        .split('/')
        .map(|seg| urlencoding::encode(seg).into_owned())
        .collect::<Vec<_>>()
        .join("/");
    match version_id {
        // Version ids are opaque (B2/others may use '+', '='); encode like the key.
        Some(v) => format!("{bucket}/{encoded_key}?versionId={}", urlencoding::encode(v)),
        None => format!("{bucket}/{encoded_key}"),
    }
}

fn part_ranges(size: i64) -> Vec<(i32, i64, i64)> {
    let part_size = MIN_PART.max((size + MAX_PARTS - 1) / MAX_PARTS);
    let mut out = Vec::new();
    let mut start = 0;
    let mut n = 1;
    while start < size {
        let end = (start + part_size).min(size) - 1;
        out.push((n, start, end));
        start = end + 1;
        n += 1;
    }
    out
}

impl S3Store {
    pub(super) async fn server_copy(
        &self,
        ctx: &str,
        src_bucket: &str,
        src_key: &str,
        version_id: Option<&str>,
        dst_bucket: &str,
        dst_key: &str,
    ) -> AppResult<()> {
        let source = copy_source(src_bucket, src_key, version_id);
        let head = self
            .client
            .head_object()
            .bucket(src_bucket)
            .key(src_key)
            .set_version_id(version_id.map(str::to_string))
            .send()
            .await
            .map_err(|e| classify_aws(ctx, e))?;
        let size = head.content_length().unwrap_or(0);

        if size <= SINGLE_COPY_MAX {
            self.client
                .copy_object()
                .copy_source(source)
                .bucket(dst_bucket)
                .key(dst_key)
                .send()
                .await
                .map_err(|e| classify_aws(ctx, e))?;
            return Ok(());
        }

        // Multipart copy does not carry metadata or tags over; replay them (tags best-effort).
        let tagging = self
            .client
            .get_object_tagging()
            .bucket(src_bucket)
            .key(src_key)
            .set_version_id(version_id.map(str::to_string))
            .send()
            .await
            .ok()
            .map(|t| {
                t.tag_set()
                    .iter()
                    .map(|t| format!("{}={}", urlencoding::encode(t.key()), urlencoding::encode(t.value())))
                    .collect::<Vec<_>>()
                    .join("&")
            })
            .filter(|s| !s.is_empty());
        let created = self
            .client
            .create_multipart_upload()
            .bucket(dst_bucket)
            .key(dst_key)
            .set_content_type(head.content_type().map(str::to_string))
            .set_cache_control(head.cache_control().map(str::to_string))
            .set_content_disposition(head.content_disposition().map(str::to_string))
            .set_content_encoding(head.content_encoding().map(str::to_string))
            .set_content_language(head.content_language().map(str::to_string))
            .set_metadata(head.metadata().cloned())
            .set_storage_class(head.storage_class().cloned())
            .set_tagging(tagging)
            .send()
            .await
            .map_err(|e| classify_aws(ctx, e))?;
        let upload_id = created
            .upload_id()
            .ok_or_else(|| AppError::S3(format!("{ctx}: no upload id")))?
            .to_string();

        let etag = head.e_tag().map(str::to_string);
        let result = self
            .copy_parts(ctx, &source, etag.as_deref(), size, dst_bucket, dst_key, &upload_id)
            .await;
        if result.is_err() {
            // Don't leave billable orphan parts behind.
            let _ = self
                .client
                .abort_multipart_upload()
                .bucket(dst_bucket)
                .key(dst_key)
                .upload_id(&upload_id)
                .send()
                .await;
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn copy_parts(
        &self,
        ctx: &str,
        source: &str,
        src_etag: Option<&str>,
        size: i64,
        dst_bucket: &str,
        dst_key: &str,
        upload_id: &str,
    ) -> AppResult<()> {
        let mut parts: Vec<CompletedPart> = stream::iter(part_ranges(size))
            .map(|(n, start, end)| async move {
                // if-match pins every part to the same source bytes.
                let resp = self
                    .client
                    .upload_part_copy()
                    .bucket(dst_bucket)
                    .key(dst_key)
                    .upload_id(upload_id)
                    .part_number(n)
                    .copy_source(source)
                    .copy_source_range(format!("bytes={start}-{end}"))
                    .set_copy_source_if_match(src_etag.map(str::to_string))
                    .send()
                    .await
                    .map_err(|e| classify_aws(ctx, e))?;
                let etag = resp
                    .copy_part_result()
                    .and_then(|r| r.e_tag())
                    .ok_or_else(|| AppError::S3(format!("{ctx}: part {n} returned no etag")))?;
                Ok::<_, AppError>(CompletedPart::builder().part_number(n).e_tag(etag).build())
            })
            .buffer_unordered(PART_CONCURRENCY)
            .try_collect()
            .await?;
        parts.sort_by_key(|p| p.part_number());

        self.client
            .complete_multipart_upload()
            .bucket(dst_bucket)
            .key(dst_key)
            .upload_id(upload_id)
            .multipart_upload(CompletedMultipartUpload::builder().set_parts(Some(parts)).build())
            .send()
            .await
            .map_err(|e| classify_aws(ctx, e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_ranges_cover_size_contiguously() {
        let size = 6 * 1024 * 1024 * 1024 + 7;
        let r = part_ranges(size);
        assert_eq!(r.first().map(|p| (p.0, p.1)), Some((1, 0)));
        assert_eq!(r.last().map(|p| p.2), Some(size - 1));
        for w in r.windows(2) {
            assert_eq!(w[1].1, w[0].2 + 1);
            assert_eq!(w[1].0, w[0].0 + 1);
        }
    }

    #[test]
    fn part_ranges_stay_under_part_cap_for_5tb() {
        let size: i64 = 5 * 1024 * 1024 * 1024 * 1024;
        assert!(part_ranges(size).len() as i64 <= MAX_PARTS);
    }

    #[test]
    fn copy_source_encodes_segments_and_version() {
        assert_eq!(copy_source("b", "a b/c.txt", None), "b/a%20b/c.txt");
        assert_eq!(copy_source("b", "k", Some("v1")), "b/k?versionId=v1");
        assert_eq!(copy_source("b", "a+b/\u{e9}", Some("x+y=")), "b/a%2Bb/%C3%A9?versionId=x%2By%3D");
    }
}
