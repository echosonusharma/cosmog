//! [`ObjectStore`] wrapper that transparently recovers from S3
//! `PermanentRedirect` (HTTP 301) errors on accounts whose buckets span
//! multiple regions.
//!
//! AWS S3 requires requests to be signed for the bucket's own region. A single
//! account-level region therefore cannot serve a multi-region account. This
//! wrapper keeps a per-bucket region map: when any operation fails with
//! [`AppError::RegionRedirect`], it probes the bucket's real region via
//! `GetBucketLocation` on the global endpoint, builds (and caches) a client
//! for that region, and retries the operation once. Subsequent calls for the
//! same bucket go straight to the regional client.
//!
//! Only meaningful for real AWS (no custom endpoint) — callers should not wrap
//! stores for endpoint-configured providers (R2, B2, MinIO, ...).

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;

use crate::db::accounts::Account;
use crate::error::{AppError, AppResult};
use crate::providers::{build_probe_store, build_store_with_region};
use crate::store::{
    Bucket, CannedAcl, DeleteObjectsResult, GetOptions, ListOptions, ListPage, ObjectMeta,
    ObjectPreview, ObjectStore, ObjectTag, ObjectVersion, PendingMultipartUpload, PutOptions,
};
use crate::transfer::{DownloadResult, TransferCtx, UploadResult};

pub struct RegionRetryStore {
    default: Arc<dyn ObjectStore>,
    account: Account,
    /// bucket name -> detected region.
    bucket_region: DashMap<String, String>,
    /// region -> client signed for that region.
    clients_by_region: DashMap<String, Arc<dyn ObjectStore>>,
}

impl RegionRetryStore {
    pub fn new(default: Arc<dyn ObjectStore>, account: Account) -> Self {
        Self {
            default,
            account,
            bucket_region: DashMap::new(),
            clients_by_region: DashMap::new(),
        }
    }

    async fn client_for_region(&self, region: &str) -> AppResult<Arc<dyn ObjectStore>> {
        if region == self.account.region {
            return Ok(self.default.clone());
        }
        if let Some(c) = self.clients_by_region.get(region) {
            return Ok(c.clone());
        }
        let built = build_store_with_region(&self.account, region).await?;
        Ok(self
            .clients_by_region
            .entry(region.to_string())
            .or_insert(built)
            .clone())
    }

    /// Client to use for `bucket`: regional if we already know its region,
    /// otherwise the account default.
    async fn resolve(&self, bucket: &str) -> AppResult<Arc<dyn ObjectStore>> {
        let region = self.bucket_region.get(bucket).map(|r| r.clone());
        match region {
            Some(r) => self.client_for_region(&r).await,
            None => Ok(self.default.clone()),
        }
    }

    /// Called after a `RegionRedirect`: detect the bucket's real region,
    /// remember it, and return a client for it.
    ///
    /// A probe *failure* (e.g. IAM policy missing `s3:GetBucketLocation`)
    /// propagates instead of falling back to a guessed region — caching a
    /// wrong region would make every subsequent call for the bucket fail with
    /// two extra requests. `Ok(None)` legitimately means us-east-1 (empty
    /// `LocationConstraint`).
    async fn recover(&self, bucket: &str) -> AppResult<Arc<dyn ObjectStore>> {
        let probe = build_probe_store(&self.account).await?;
        let region = probe
            .get_bucket_location(bucket)
            .await
            .map_err(|e| {
                tracing::warn!(bucket = %bucket, "GetBucketLocation probe failed: {e}");
                e
            })?
            .unwrap_or_else(|| "us-east-1".to_string());
        tracing::info!(
            account_id = %self.account.id,
            bucket = %bucket,
            region = %region,
            "PermanentRedirect: routing bucket to its own region"
        );
        self.bucket_region.insert(bucket.to_string(), region.clone());
        self.client_for_region(&region).await
    }
}

/// Run `$call` against the bucket's resolved client; on `RegionRedirect`,
/// detect the real region and retry once. `$call` must be re-evaluable
/// (clone any by-value args).
macro_rules! with_retry {
    ($self:ident, $bucket:expr, $store:ident, $call:expr) => {{
        let $store = $self.resolve($bucket).await?;
        match $call {
            Err(AppError::RegionRedirect(_)) => {
                let $store = $self.recover($bucket).await?;
                $call
            }
            r => r,
        }
    }};
}

#[async_trait]
impl ObjectStore for RegionRetryStore {
    async fn list_buckets(&self) -> AppResult<Vec<Bucket>> {
        self.default.list_buckets().await
    }

    async fn create_bucket(&self, name: &str, region: Option<&str>) -> AppResult<()> {
        self.default.create_bucket(name, region).await
    }

    async fn delete_bucket(&self, name: &str) -> AppResult<()> {
        with_retry!(self, name, s, s.delete_bucket(name).await)
    }

    async fn head_bucket(&self, name: &str) -> AppResult<()> {
        with_retry!(self, name, s, s.head_bucket(name).await)
    }

    async fn get_bucket_location(&self, name: &str) -> AppResult<Option<String>> {
        self.default.get_bucket_location(name).await
    }

    async fn put_bucket_acl(&self, name: &str, acl: CannedAcl) -> AppResult<()> {
        with_retry!(self, name, s, s.put_bucket_acl(name, acl.clone()).await)
    }

    async fn get_bucket_versioning(&self, name: &str) -> AppResult<bool> {
        with_retry!(self, name, s, s.get_bucket_versioning(name).await)
    }

    async fn put_bucket_versioning(&self, name: &str, enabled: bool) -> AppResult<()> {
        with_retry!(self, name, s, s.put_bucket_versioning(name, enabled).await)
    }

    async fn list_objects(&self, bucket: &str, opts: ListOptions) -> AppResult<ListPage> {
        with_retry!(self, bucket, s, s.list_objects(bucket, opts.clone()).await)
    }

    async fn head_object(&self, bucket: &str, key: &str) -> AppResult<ObjectMeta> {
        with_retry!(self, bucket, s, s.head_object(bucket, key).await)
    }

    async fn create_folder(&self, bucket: &str, prefix: &str) -> AppResult<()> {
        with_retry!(self, bucket, s, s.create_folder(bucket, prefix).await)
    }

    async fn delete_object(&self, bucket: &str, key: &str) -> AppResult<()> {
        with_retry!(self, bucket, s, s.delete_object(bucket, key).await)
    }

    async fn delete_objects(
        &self,
        bucket: &str,
        keys: &[String],
    ) -> AppResult<DeleteObjectsResult> {
        with_retry!(self, bucket, s, s.delete_objects(bucket, keys).await)
    }

    async fn delete_object_version(
        &self,
        bucket: &str,
        key: &str,
        version_id: &str,
    ) -> AppResult<()> {
        with_retry!(self, bucket, s, s.delete_object_version(bucket, key, version_id).await)
    }

    async fn list_object_versions(
        &self,
        bucket: &str,
        prefix: Option<&str>,
        continuation: Option<String>,
    ) -> AppResult<(Vec<ObjectVersion>, Option<String>)> {
        with_retry!(
            self, bucket, s,
            s.list_object_versions(bucket, prefix, continuation.clone()).await
        )
    }

    async fn copy_object(
        &self,
        src_bucket: &str,
        src_key: &str,
        dst_bucket: &str,
        dst_key: &str,
    ) -> AppResult<()> {
        // CopyObject executes against the destination bucket's region.
        with_retry!(
            self, dst_bucket, s,
            s.copy_object(src_bucket, src_key, dst_bucket, dst_key).await
        )
    }

    async fn put_object_acl(&self, bucket: &str, key: &str, acl: CannedAcl) -> AppResult<()> {
        with_retry!(self, bucket, s, s.put_object_acl(bucket, key, acl.clone()).await)
    }

    async fn presign_get(&self, bucket: &str, key: &str, expires_secs: u64) -> AppResult<String> {
        // Presigning is local — no request is made, so a wrong-region URL
        // would fail only later in the recipient's browser. If we don't know
        // the bucket's region yet, probe it first (best-effort) so the URL
        // points at the right endpoint.
        if !self.bucket_region.contains_key(bucket) {
            match build_probe_store(&self.account).await {
                Ok(probe) => match probe.get_bucket_location(bucket).await {
                    Ok(loc) => {
                        let region = loc.unwrap_or_else(|| "us-east-1".to_string());
                        self.bucket_region.insert(bucket.to_string(), region);
                    }
                    Err(e) => {
                        tracing::warn!(bucket = %bucket, "presign region probe failed, using account region: {e}");
                    }
                },
                Err(e) => tracing::warn!("presign probe store build failed: {e}"),
            }
        }
        let s = self.resolve(bucket).await?;
        s.presign_get(bucket, key, expires_secs).await
    }

    async fn read_object_range(
        &self,
        bucket: &str,
        key: &str,
        max_bytes: u64,
    ) -> AppResult<ObjectPreview> {
        with_retry!(self, bucket, s, s.read_object_range(bucket, key, max_bytes).await)
    }

    async fn read_object_full(&self, bucket: &str, key: &str) -> AppResult<Vec<u8>> {
        with_retry!(self, bucket, s, s.read_object_full(bucket, key).await)
    }

    async fn get_object_tagging(&self, bucket: &str, key: &str) -> AppResult<Vec<ObjectTag>> {
        with_retry!(self, bucket, s, s.get_object_tagging(bucket, key).await)
    }

    async fn put_object_tagging(
        &self,
        bucket: &str,
        key: &str,
        tags: &[ObjectTag],
    ) -> AppResult<()> {
        with_retry!(self, bucket, s, s.put_object_tagging(bucket, key, tags).await)
    }

    async fn delete_object_tagging(&self, bucket: &str, key: &str) -> AppResult<()> {
        with_retry!(self, bucket, s, s.delete_object_tagging(bucket, key).await)
    }

    async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        source: PathBuf,
        opts: PutOptions,
        ctx: TransferCtx,
    ) -> AppResult<UploadResult> {
        with_retry!(
            self, bucket, s,
            s.put_object(bucket, key, source.clone(), opts.clone(), ctx.clone()).await
        )
    }

    async fn put_object_bytes(
        &self,
        bucket: &str,
        key: &str,
        content_type: &str,
        data: Vec<u8>,
        user_metadata: std::collections::HashMap<String, String>,
    ) -> AppResult<()> {
        with_retry!(
            self, bucket, s,
            s.put_object_bytes(bucket, key, content_type, data.clone(), user_metadata.clone()).await
        )
    }

    async fn get_object(
        &self,
        bucket: &str,
        key: &str,
        dest: PathBuf,
        opts: GetOptions,
        ctx: TransferCtx,
    ) -> AppResult<DownloadResult> {
        with_retry!(
            self, bucket, s,
            s.get_object(bucket, key, dest.clone(), opts.clone(), ctx.clone()).await
        )
    }

    async fn abort_multipart_upload(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
    ) -> AppResult<()> {
        with_retry!(self, bucket, s, s.abort_multipart_upload(bucket, key, upload_id).await)
    }

    async fn list_multipart_uploads(
        &self,
        bucket: &str,
        prefix: Option<&str>,
        key_marker: Option<String>,
    ) -> AppResult<(Vec<PendingMultipartUpload>, Option<String>)> {
        with_retry!(
            self, bucket, s,
            s.list_multipart_uploads(bucket, prefix, key_marker.clone()).await
        )
    }
}
