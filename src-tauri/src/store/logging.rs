//! Transparent [`ObjectStore`] wrapper that records every operation to the
//! `request_logs` SQLite table. Each method delegates to the inner store then
//! fire-and-forgets a DB insert so the caller's latency is unaffected.
//!
//! `list_objects` is logged but deduplicated per (bucket, prefix) with a 10s
//! cooldown to avoid flooding from the 1.5s browse poll.
//! `get_bucket_location` is not logged (internal probe only).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use dashmap::DashMap;
use serde_json::{json, Value};
use tauri::Emitter;

use crate::db::request_logs::NewRequestLog;
use crate::db::Db;
use crate::error::AppResult;
use crate::store::{
    Bucket, CannedAcl, DeleteObjectsResult, GetOptions, ListOptions, ListPage, ObjectMeta,
    ObjectPreview, ObjectStore, ObjectTag, ObjectVersion, PendingMultipartUpload, PutOptions,
};
use crate::transfer::{DownloadResult, TransferCtx, UploadResult};

/// Minimum interval between logged `list_objects` calls for the same
/// (bucket, prefix) pair. Prevents the 1.5s poll from flooding the table.
const LIST_OBJECTS_LOG_COOLDOWN: Duration = Duration::from_secs(10);

pub struct LoggingStore {
    inner: Arc<dyn ObjectStore>,
    db: Db,
    app: tauri::AppHandle,
    account_id: String,
    account_name: String,
    /// Raw endpoint URL as configured by the user, or None for AWS S3.
    endpoint: Option<String>,
    /// Signing region (e.g. "us-east-1", "auto").
    region: String,
    /// Tracks when we last logged a `list_objects` for each (bucket, prefix).
    list_objects_last_logged: Arc<DashMap<(String, String), Instant>>,
}

impl LoggingStore {
    pub fn new(
        inner: Arc<dyn ObjectStore>,
        db: Db,
        app: tauri::AppHandle,
        account_id: impl Into<String>,
        account_name: impl Into<String>,
        endpoint: Option<String>,
        region: impl Into<String>,
    ) -> Self {
        Self {
            inner,
            db,
            app,
            account_id: account_id.into(),
            account_name: account_name.into(),
            endpoint,
            region: region.into(),
            list_objects_last_logged: Arc::new(DashMap::new()),
        }
    }

    /// Construct the HTTP request URL for an operation.
    fn build_url(&self, bucket: Option<&str>, key: Option<&str>, query: Option<&str>) -> String {
        let base = if let Some(ep) = &self.endpoint {
            ep.trim_end_matches('/').to_string()
        } else {
            format!("https://s3.{}.amazonaws.com", self.region)
        };
        let mut url = base;
        if let Some(b) = bucket {
            url.push('/');
            url.push_str(b);
        }
        if let Some(k) = key {
            url.push('/');
            url.push_str(k);
        }
        if let Some(q) = query {
            url.push('?');
            url.push_str(q);
        }
        url
    }

    /// Infer the HTTP response status for a successful call.
    fn success_status(op: &str) -> i64 {
        match op {
            "delete_object" | "delete_object_version" | "delete_object_tagging"
            | "abort_multipart_upload" => 204,
            "head_bucket" | "head_object" => 200,
            "delete_objects" => 200, // S3 DeleteObjects is a POST, returns 200 with XML body
            _ => 200,
        }
    }

    /// Map an AppError code to the most likely HTTP status code.
    fn error_http_status(code: &str) -> Option<i64> {
        match code {
            "not_found" => Some(404),
            "access_denied" => Some(403),
            "credentials_invalid" => Some(403),
            "conflict" => Some(409),
            "rate_limited" => Some(429),
            "region_redirect" => Some(301),
            "internal" => Some(500),
            _ => None,
        }
    }

    fn fire_log(&self, log: NewRequestLog) {
        let db = self.db.clone();
        let app = self.app.clone();
        tokio::spawn(async move {
            if let Err(e) = db.insert_request_log(log).await {
                tracing::warn!("request log insert failed: {e}");
                return;
            }
            let _ = app.emit("request-log-added", ());
        });
    }

    fn record(
        &self,
        op: &'static str,
        http_method: &'static str,
        url: String,
        params: Option<Value>,
        response_meta: Option<Value>,
        bucket: Option<&str>,
        key: Option<&str>,
        start: Instant,
        ok: bool,
        err: Option<&crate::error::AppError>,
    ) {
        let duration_ms = start.elapsed().as_millis() as i64;
        let (status, response_status, error_code, error_msg) = if ok {
            ("ok".to_string(), Some(Self::success_status(op)), None, None)
        } else if let Some(e) = err {
            let code = e.code();
            (
                "error".to_string(),
                Self::error_http_status(code),
                Some(code.to_string()),
                Some(e.to_string()),
            )
        } else {
            ("error".to_string(), None, None, None)
        };
        self.fire_log(NewRequestLog {
            account_id: Some(self.account_id.clone()),
            account_name: Some(self.account_name.clone()),
            operation: op.to_string(),
            http_method: Some(http_method.to_string()),
            request_url: Some(url),
            request_params: params.map(|p| p.to_string()),
            response_meta: response_meta.map(|m| m.to_string()),
            bucket: bucket.map(String::from),
            key: key.map(String::from),
            status,
            response_status,
            error_code,
            error_msg,
            duration_ms,
        });
    }
}

#[async_trait]
impl ObjectStore for LoggingStore {
    async fn list_buckets(&self) -> AppResult<Vec<Bucket>> {
        let start = Instant::now();
        let r = self.inner.list_buckets().await;
        let meta = r.as_ref().ok().map(|v| json!({ "count": v.len() }));
        self.record("list_buckets", "GET", self.build_url(None, None, None),
            None, meta, None, None, start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn create_bucket(&self, name: &str, region: Option<&str>) -> AppResult<()> {
        let start = Instant::now();
        let r = self.inner.create_bucket(name, region).await;
        self.record("create_bucket", "PUT", self.build_url(Some(name), None, None),
            Some(json!({ "region": region })), None, Some(name), None,
            start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn delete_bucket(&self, name: &str) -> AppResult<()> {
        let start = Instant::now();
        let r = self.inner.delete_bucket(name).await;
        self.record("delete_bucket", "DELETE", self.build_url(Some(name), None, None),
            None, None, Some(name), None, start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn head_bucket(&self, name: &str) -> AppResult<()> {
        let start = Instant::now();
        let r = self.inner.head_bucket(name).await;
        self.record("head_bucket", "HEAD", self.build_url(Some(name), None, None),
            None, None, Some(name), None, start, r.is_ok(), r.as_ref().err());
        r
    }

    // Not logged — internal probe, not user-facing.
    async fn get_bucket_location(&self, name: &str) -> AppResult<Option<String>> {
        self.inner.get_bucket_location(name).await
    }

    async fn put_bucket_acl(&self, name: &str, acl: CannedAcl) -> AppResult<()> {
        let start = Instant::now();
        let r = self.inner.put_bucket_acl(name, acl.clone()).await;
        self.record("put_bucket_acl", "PUT", self.build_url(Some(name), None, Some("acl")),
            Some(json!({ "acl": acl.as_str() })), None, Some(name), None,
            start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn get_bucket_versioning(&self, name: &str) -> AppResult<bool> {
        let start = Instant::now();
        let r = self.inner.get_bucket_versioning(name).await;
        let meta = r.as_ref().ok().map(|&enabled| json!({ "enabled": enabled }));
        self.record("get_bucket_versioning", "GET",
            self.build_url(Some(name), None, Some("versioning")),
            None, meta, Some(name), None, start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn put_bucket_versioning(&self, name: &str, enabled: bool) -> AppResult<()> {
        let start = Instant::now();
        let r = self.inner.put_bucket_versioning(name, enabled).await;
        self.record("put_bucket_versioning", "PUT",
            self.build_url(Some(name), None, Some("versioning")),
            Some(json!({ "enabled": enabled })), None, Some(name), None,
            start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn list_objects(&self, bucket: &str, opts: ListOptions) -> AppResult<ListPage> {
        let prefix = opts.prefix.clone().unwrap_or_default();
        let cache_key = (bucket.to_string(), prefix.clone());
        let should_log = {
            let now = Instant::now();
            match self.list_objects_last_logged.entry(cache_key) {
                dashmap::mapref::entry::Entry::Vacant(v) => {
                    // First list for this (bucket, prefix) — always log.
                    v.insert(now);
                    true
                }
                dashmap::mapref::entry::Entry::Occupied(mut o) => {
                    if now.duration_since(*o.get()) >= LIST_OBJECTS_LOG_COOLDOWN {
                        o.insert(now);
                        true
                    } else {
                        false
                    }
                }
            }
        };
        let start = Instant::now();
        let r = self.inner.list_objects(bucket, opts).await;
        if should_log {
            let meta = r.as_ref().ok().map(|p| json!({
                "objects": p.objects.len(),
                "prefixes": p.prefixes.len(),
                "truncated": p.is_truncated,
            }));
            self.record("list_objects", "GET",
                self.build_url(Some(bucket), None, Some(&format!("list-type=2&prefix={prefix}&delimiter=%2F"))),
                Some(json!({ "prefix": prefix })), meta, Some(bucket), None,
                start, r.is_ok(), r.as_ref().err());
        }
        r
    }

    async fn head_object(&self, bucket: &str, key: &str) -> AppResult<ObjectMeta> {
        let start = Instant::now();
        let r = self.inner.head_object(bucket, key).await;
        let meta = r.as_ref().ok().map(|m| json!({
            "size": m.size,
            "content_type": m.content_type,
            "etag": m.etag,
            "last_modified": m.last_modified,
            "storage_class": m.storage_class,
        }));
        self.record("head_object", "HEAD", self.build_url(Some(bucket), Some(key), None),
            None, meta, Some(bucket), Some(key), start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn create_folder(&self, bucket: &str, prefix: &str) -> AppResult<()> {
        let start = Instant::now();
        let r = self.inner.create_folder(bucket, prefix).await;
        self.record("create_folder", "PUT", self.build_url(Some(bucket), Some(prefix), None),
            Some(json!({ "content_length": 0 })), None, Some(bucket), Some(prefix),
            start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn delete_object(&self, bucket: &str, key: &str) -> AppResult<()> {
        let start = Instant::now();
        let r = self.inner.delete_object(bucket, key).await;
        self.record("delete_object", "DELETE", self.build_url(Some(bucket), Some(key), None),
            None, None, Some(bucket), Some(key), start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn delete_objects(&self, bucket: &str, keys: &[String]) -> AppResult<DeleteObjectsResult> {
        let start = Instant::now();
        let count = keys.len();
        let sample: Vec<&str> = keys.iter().take(5).map(|s| s.as_str()).collect();
        let r = self.inner.delete_objects(bucket, keys).await;
        let meta = r.as_ref().ok().map(|res| json!({
            "deleted": res.deleted.len(),
            "errors": res.errors.len(),
        }));
        self.record("delete_objects", "POST",
            self.build_url(Some(bucket), None, Some("delete")),
            Some(json!({ "key_count": count, "keys_sample": sample })), meta,
            Some(bucket), None, start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn delete_object_version(
        &self, bucket: &str, key: &str, version_id: &str,
    ) -> AppResult<()> {
        let start = Instant::now();
        let r = self.inner.delete_object_version(bucket, key, version_id).await;
        self.record("delete_object_version", "DELETE",
            self.build_url(Some(bucket), Some(key), Some(&format!("versionId={version_id}"))),
            Some(json!({ "version_id": version_id })), None, Some(bucket), Some(key),
            start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn list_object_versions(
        &self, bucket: &str, prefix: Option<&str>, continuation: Option<String>,
    ) -> AppResult<(Vec<ObjectVersion>, Option<String>)> {
        let start = Instant::now();
        let r = self.inner.list_object_versions(bucket, prefix, continuation.clone()).await;
        let meta = r.as_ref().ok().map(|(versions, next)| json!({
            "versions": versions.len(),
            "has_more": next.is_some(),
        }));
        self.record("list_object_versions", "GET",
            self.build_url(Some(bucket), None, Some("versions")),
            Some(json!({ "prefix": prefix, "has_continuation": continuation.is_some() })),
            meta, Some(bucket), prefix, start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn copy_object(
        &self, src_bucket: &str, src_key: &str, dst_bucket: &str, dst_key: &str,
    ) -> AppResult<()> {
        let start = Instant::now();
        let r = self.inner.copy_object(src_bucket, src_key, dst_bucket, dst_key).await;
        self.record("copy_object", "PUT",
            self.build_url(Some(dst_bucket), Some(dst_key), None),
            Some(json!({
                "copy_source": format!("{src_bucket}/{src_key}"),
                "src_bucket": src_bucket, "src_key": src_key,
                "dst_bucket": dst_bucket, "dst_key": dst_key,
            // bucket/key = destination: that's where the PUT actually goes,
            // matching request_url; the source is in request_params.
            })), None, Some(dst_bucket), Some(dst_key),
            start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn put_object_acl(&self, bucket: &str, key: &str, acl: CannedAcl) -> AppResult<()> {
        let start = Instant::now();
        let r = self.inner.put_object_acl(bucket, key, acl.clone()).await;
        self.record("put_object_acl", "PUT",
            self.build_url(Some(bucket), Some(key), Some("acl")),
            Some(json!({ "acl": acl.as_str() })), None, Some(bucket), Some(key),
            start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn presign_get(&self, bucket: &str, key: &str, expires_secs: u64) -> AppResult<String> {
        let start = Instant::now();
        let r = self.inner.presign_get(bucket, key, expires_secs).await;
        // presign is a local signing operation — no network request made
        self.record("presign_get", "LOCAL",
            self.build_url(Some(bucket), Some(key), None),
            Some(json!({ "expires_secs": expires_secs })),
            None, Some(bucket), Some(key), start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn read_object_range(
        &self, bucket: &str, key: &str, max_bytes: u64,
    ) -> AppResult<ObjectPreview> {
        let start = Instant::now();
        let r = self.inner.read_object_range(bucket, key, max_bytes).await;
        let meta = r.as_ref().ok().map(|p| json!({
            "bytes_read": p.bytes.len(),
            "total_size": p.total_size,
            "truncated": p.truncated,
            "content_type": p.content_type,
        }));
        self.record("read_object_range", "GET",
            self.build_url(Some(bucket), Some(key), None),
            Some(json!({ "range": format!("bytes=0-{}", max_bytes.saturating_sub(1)), "max_bytes": max_bytes })),
            meta, Some(bucket), Some(key), start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn read_object_full(&self, bucket: &str, key: &str) -> AppResult<Vec<u8>> {
        let start = Instant::now();
        let r = self.inner.read_object_full(bucket, key).await;
        let meta = r.as_ref().ok().map(|b| json!({ "bytes_read": b.len() }));
        self.record("read_object_full", "GET",
            self.build_url(Some(bucket), Some(key), None),
            None, meta, Some(bucket), Some(key), start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn get_object_tagging(&self, bucket: &str, key: &str) -> AppResult<Vec<ObjectTag>> {
        let start = Instant::now();
        let r = self.inner.get_object_tagging(bucket, key).await;
        let meta = r.as_ref().ok().map(|tags| json!({ "tag_count": tags.len() }));
        self.record("get_object_tagging", "GET",
            self.build_url(Some(bucket), Some(key), Some("tagging")),
            None, meta, Some(bucket), Some(key), start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn put_object_tagging(
        &self, bucket: &str, key: &str, tags: &[ObjectTag],
    ) -> AppResult<()> {
        let start = Instant::now();
        let tag_pairs: Vec<_> = tags.iter().map(|t| json!({ "key": t.key, "value": t.value })).collect();
        let r = self.inner.put_object_tagging(bucket, key, tags).await;
        self.record("put_object_tagging", "PUT",
            self.build_url(Some(bucket), Some(key), Some("tagging")),
            Some(json!({ "tag_count": tags.len(), "tags": tag_pairs })), None,
            Some(bucket), Some(key), start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn delete_object_tagging(&self, bucket: &str, key: &str) -> AppResult<()> {
        let start = Instant::now();
        let r = self.inner.delete_object_tagging(bucket, key).await;
        self.record("delete_object_tagging", "DELETE",
            self.build_url(Some(bucket), Some(key), Some("tagging")),
            None, None, Some(bucket), Some(key), start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn put_object(
        &self, bucket: &str, key: &str, source: PathBuf, opts: PutOptions, ctx: TransferCtx,
    ) -> AppResult<UploadResult> {
        let start = Instant::now();
        let params = json!({
            "content_type": opts.content_type,
            "acl": opts.acl.as_ref().map(|a| a.as_str()),
            "cache_control": opts.cache_control,
            "sse": opts.sse.is_some(),
            "source_path": source.display().to_string(),
        });
        let r = self.inner.put_object(bucket, key, source, opts, ctx).await;
        let meta = r.as_ref().ok().map(|res| json!({
            "etag": res.etag,
            "multipart": res.upload_id.is_some(),
        }));
        self.record("put_object", "PUT", self.build_url(Some(bucket), Some(key), None),
            Some(params), meta, Some(bucket), Some(key), start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn put_object_bytes(
        &self, bucket: &str, key: &str, content_type: &str, data: Vec<u8>,
        user_metadata: std::collections::HashMap<String, String>,
    ) -> AppResult<()> {
        let start = Instant::now();
        let size = data.len();
        let r = self.inner.put_object_bytes(bucket, key, content_type, data, user_metadata).await;
        self.record("put_object_bytes", "PUT", self.build_url(Some(bucket), Some(key), None),
            Some(json!({ "content_type": content_type, "content_length": size })), None,
            Some(bucket), Some(key), start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn get_object(
        &self, bucket: &str, key: &str, dest: PathBuf, opts: GetOptions, ctx: TransferCtx,
    ) -> AppResult<DownloadResult> {
        let start = Instant::now();
        let params = json!({
            "version_id": opts.version_id,
            "range": match (opts.range_start, opts.range_end) {
                (Some(s), Some(e)) => Some(format!("bytes={s}-{e}")),
                (Some(s), None)    => Some(format!("bytes={s}-")),
                (None, Some(e))    => Some(format!("bytes=0-{e}")),
                (None, None)       => None::<String>,
            },
            "dest_path": dest.display().to_string(),
        });
        let r = self.inner.get_object(bucket, key, dest, opts, ctx).await;
        let meta = r.as_ref().ok().map(|res| json!({
            "bytes": res.bytes,
        }));
        self.record("get_object", "GET", self.build_url(Some(bucket), Some(key), None),
            Some(params), meta, Some(bucket), Some(key), start, r.is_ok(), r.as_ref().err());
        r
    }

    async fn abort_multipart_upload(
        &self, bucket: &str, key: &str, upload_id: &str,
    ) -> AppResult<()> {
        let start = Instant::now();
        let r = self.inner.abort_multipart_upload(bucket, key, upload_id).await;
        self.record("abort_multipart_upload", "DELETE",
            self.build_url(Some(bucket), Some(key), Some(&format!("uploadId={upload_id}"))),
            Some(json!({ "upload_id": upload_id })), None, Some(bucket), Some(key),
            start, r.is_ok(), r.as_ref().err());
        r
    }

    // Not logged — UI polling for multipart upload list.
    async fn list_multipart_uploads(
        &self, bucket: &str, prefix: Option<&str>, key_marker: Option<String>,
    ) -> AppResult<(Vec<PendingMultipartUpload>, Option<String>)> {
        self.inner.list_multipart_uploads(bucket, prefix, key_marker).await
    }
}
