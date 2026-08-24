//! Protocol-neutral storage abstraction; [`ObjectStore`] is the single trait the backend depends on.
//! Providers live in submodules (currently `s3`); adding one = new submodule + trait impl wired via [`crate::providers::Protocol`].

pub mod logging;
pub mod region_retry;
pub mod s3;

#[cfg(target_os = "android")]
pub mod android_tls;

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::AppResult;
use crate::transfer::{DownloadResult, TransferCtx, UploadResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bucket {
    pub name: String,
    pub created_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectMeta {
    pub key: String,
    pub size: i64,
    pub etag: Option<String>,
    pub last_modified: Option<i64>,
    pub storage_class: Option<String>,
    pub content_type: Option<String>,
    pub version_id: Option<String>,
    /// Raw metadata names (no `x-amz-meta-` prefix); populated only by `head_object`.
    #[serde(default)]
    pub user_metadata: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectVersion {
    pub key: String,
    pub version_id: Option<String>,
    pub is_latest: bool,
    pub is_delete_marker: bool,
    pub size: Option<i64>,
    pub etag: Option<String>,
    pub last_modified: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListPage {
    pub objects: Vec<ObjectMeta>,
    pub prefixes: Vec<String>,
    pub continuation: Option<String>,
    pub is_truncated: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListOptions {
    pub prefix: Option<String>,
    pub delimiter: Option<String>,
    pub continuation: Option<String>,
    pub max_keys: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CannedAcl {
    Private,
    PublicRead,
}

impl CannedAcl {
    pub fn as_str(&self) -> &'static str {
        match self {
            CannedAcl::Private => "private",
            CannedAcl::PublicRead => "public-read",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PutOptions {
    pub content_type: Option<String>,
    pub acl: Option<CannedAcl>,
    pub cache_control: Option<String>,
    pub content_disposition: Option<String>,
    pub content_encoding: Option<String>,
    /// Sent as `x-amz-meta-<key>`; do NOT include the prefix in keys here.
    #[serde(default)]
    pub user_metadata: std::collections::HashMap<String, String>,
    pub if_match: Option<String>,
    pub if_none_match: Option<String>,
    /// Deleted after successful upload (encrypted temp source); never serialized to DB/IPC.
    #[serde(skip)]
    pub cleanup_path: Option<std::path::PathBuf>,
    /// SAF staging dir removed once the upload settles (Done/Canceled); serialized
    /// so a retry recovers it. `None` for desktop uploads.
    #[serde(default)]
    pub stage_cleanup_dir: Option<std::path::PathBuf>,
    /// Server-side encryption: `None` default, `Sse::S3` AES256, `Sse::Kms` SSE-KMS.
    /// SSE-C deliberately unexposed (needs secure key transport).
    pub sse: Option<Sse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Sse {
    S3,
    /// `key_id` may be `None` for the AWS-managed default key.
    Kms { key_id: Option<String> },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetOptions {
    pub version_id: Option<String>,
    /// Inclusive `Range:` start; `None` + `range_end: None` = full object.
    pub range_start: Option<u64>,
    /// Inclusive `Range:` end; setting only this requests `0..=range_end`.
    pub range_end: Option<u64>,
    /// Resume signal set only by the transfer manager after confirming a prior partial;
    /// otherwise a pre-existing unrelated file silently gains appended bytes.
    #[serde(default)]
    pub resume: bool,
}

/// `deleted` = keys confirmed gone; `errors` = per-key failures (caller decides handling).
#[derive(Debug, Clone, Serialize)]
pub struct DeleteObjectsResult {
    pub deleted: Vec<String>,
    pub errors: Vec<DeleteObjectError>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeleteObjectError {
    pub key: String,
    pub code: Option<String>,
    pub message: Option<String>,
}

/// Key must match `^[\w +\-=.:/@]{1,128}$` per S3 spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectTag {
    pub key: String,
    pub value: String,
}

/// One bucket CORS rule (mirrors S3 `CORSRule`); serializes snake_case.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CorsRule {
    pub id: Option<String>,
    pub allowed_origins: Vec<String>,
    pub allowed_methods: Vec<String>,
    pub allowed_headers: Vec<String>,
    pub expose_headers: Vec<String>,
    pub max_age_seconds: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CorsConfig {
    pub rules: Vec<CorsRule>,
}

/// In-memory preview; `truncated` when the object exceeds the requested cap.
#[derive(Debug, Clone, Serialize)]
pub struct ObjectPreview {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
    pub total_size: Option<i64>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PendingMultipartUpload {
    pub key: String,
    pub upload_id: String,
    pub initiated_at: Option<i64>,
}

/// The single storage trait; method names mirror S3 1:1, non-S3 providers map concepts onto them.
/// Streaming methods carry a [`TransferCtx`] (cooperative-cancel + progress hooks for the TransferManager).
#[async_trait]
pub trait ObjectStore: Send + Sync {
    async fn list_buckets(&self) -> AppResult<Vec<Bucket>>;
    /// `region: None` uses the client's default region (typical for B2-style accounts).
    async fn create_bucket(&self, name: &str, region: Option<&str>) -> AppResult<()>;
    async fn delete_bucket(&self, name: &str) -> AppResult<()>;
    async fn head_bucket(&self, name: &str) -> AppResult<()>;
    async fn get_bucket_location(&self, name: &str) -> AppResult<Option<String>>;
    async fn put_bucket_acl(&self, name: &str, acl: CannedAcl) -> AppResult<()>;
    async fn get_bucket_versioning(&self, name: &str) -> AppResult<bool>;
    async fn put_bucket_versioning(&self, name: &str, enabled: bool) -> AppResult<()>;
    /// Fetch the bucket policy JSON. Returns `Ok(None)` when no policy is set.
    async fn get_bucket_policy(&self, name: &str) -> AppResult<Option<String>>;
    async fn put_bucket_policy(&self, name: &str, policy: String) -> AppResult<()>;
    /// Delete the bucket policy. Treats "no policy present" as success.
    async fn delete_bucket_policy(&self, name: &str) -> AppResult<()>;
    /// Fetch the bucket CORS config. Returns `Ok(None)` when none is set.
    async fn get_bucket_cors(&self, name: &str) -> AppResult<Option<CorsConfig>>;
    async fn put_bucket_cors(&self, name: &str, cors: CorsConfig) -> AppResult<()>;
    /// Delete the bucket CORS config. Treats "no config present" as success.
    async fn delete_bucket_cors(&self, name: &str) -> AppResult<()>;

    async fn list_objects(&self, bucket: &str, opts: ListOptions) -> AppResult<ListPage>;
    async fn head_object(&self, bucket: &str, key: &str) -> AppResult<ObjectMeta>;
    /// Create a virtual folder by putting a zero-byte object with key `prefix/`.
    async fn create_folder(&self, bucket: &str, prefix: &str) -> AppResult<()>;
    async fn delete_object(&self, bucket: &str, key: &str) -> AppResult<()>;
    /// Deletes up to 1000 keys in one request; per-key failures land in the
    /// result's `errors` list instead of failing the whole call.
    async fn delete_objects(
        &self,
        bucket: &str,
        keys: &[String],
    ) -> AppResult<DeleteObjectsResult>;
    async fn delete_object_version(
        &self,
        bucket: &str,
        key: &str,
        version_id: &str,
    ) -> AppResult<()>;
    /// Server-side-copy a previous version onto the same key, making it latest
    /// (also un-deletes objects whose latest version is a delete marker).
    async fn restore_object_version(
        &self,
        bucket: &str,
        key: &str,
        version_id: &str,
    ) -> AppResult<()>;
    async fn list_object_versions(
        &self,
        bucket: &str,
        prefix: Option<&str>,
        continuation: Option<String>,
    ) -> AppResult<(Vec<ObjectVersion>, Option<String>)>;
    async fn copy_object(
        &self,
        src_bucket: &str,
        src_key: &str,
        dst_bucket: &str,
        dst_key: &str,
    ) -> AppResult<()>;
    async fn put_object_acl(&self, bucket: &str, key: &str, acl: CannedAcl) -> AppResult<()>;

    async fn presign_get(&self, bucket: &str, key: &str, expires_secs: u64) -> AppResult<String>;

    /// Read up to `max_bytes` into memory for FE previews; implementations may
    /// enforce a lower bound of their own.
    async fn read_object_range(
        &self,
        bucket: &str,
        key: &str,
        max_bytes: u64,
    ) -> AppResult<ObjectPreview>;

    /// Whole-object read bypassing the preview cap (AES-GCM needs full ciphertext).
    /// Callers must enforce their own size guard beforehand.
    async fn read_object_full(&self, bucket: &str, key: &str) -> AppResult<Vec<u8>>;

    /// Unsupported providers (B2) return [`AppError::InvalidInput`] so the FE hides the UI.
    async fn get_object_tagging(
        &self,
        bucket: &str,
        key: &str,
    ) -> AppResult<Vec<ObjectTag>>;
    async fn put_object_tagging(
        &self,
        bucket: &str,
        key: &str,
        tags: &[ObjectTag],
    ) -> AppResult<()>;
    async fn delete_object_tagging(&self, bucket: &str, key: &str) -> AppResult<()>;

    async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        source: PathBuf,
        opts: PutOptions,
        ctx: TransferCtx,
    ) -> AppResult<UploadResult>;

    /// Upload raw bytes (in-app text editing); `user_metadata` keys go out as
    /// `x-amz-meta-<key>` (prefix not included here).
    async fn put_object_bytes(
        &self,
        bucket: &str,
        key: &str,
        content_type: &str,
        data: Vec<u8>,
        user_metadata: std::collections::HashMap<String, String>,
    ) -> AppResult<()>;

    async fn get_object(
        &self,
        bucket: &str,
        key: &str,
        dest: PathBuf,
        opts: GetOptions,
        ctx: TransferCtx,
    ) -> AppResult<DownloadResult>;

    async fn abort_multipart_upload(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
    ) -> AppResult<()>;

    /// Paginated; next `key_marker` returned in the second tuple slot.
    async fn list_multipart_uploads(
        &self,
        bucket: &str,
        prefix: Option<&str>,
        key_marker: Option<String>,
    ) -> AppResult<(Vec<PendingMultipartUpload>, Option<String>)>;
}
