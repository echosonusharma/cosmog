//! Protocol-neutral abstraction over object storage providers.
//!
//! [`ObjectStore`] is the single trait the rest of the backend depends on. Each
//! concrete provider lives in its own submodule (currently just `s3`, which
//! covers AWS S3, Backblaze B2, Cloudflare R2, MinIO, Wasabi via endpoint
//! configuration). Adding a new protocol = adding a new submodule + trait impl
//! and wiring it through [`crate::providers::Protocol`].

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
    /// User-defined metadata as returned by HEAD. Keys are the raw metadata
    /// name (without the `x-amz-meta-` prefix). Only populated by
    /// `head_object`; `list_objects` leaves this empty.
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
    /// Maps directly onto the `Cache-Control` HTTP header on the stored
    /// object.
    pub cache_control: Option<String>,
    /// Maps onto `Content-Disposition`. Typical use: force-download with
    /// `attachment; filename="..."`.
    pub content_disposition: Option<String>,
    /// Maps onto `Content-Encoding` (e.g. `gzip`).
    pub content_encoding: Option<String>,
    /// User-defined metadata. Keys are sent as `x-amz-meta-<key>`; the
    /// `x-amz-meta-` prefix should NOT be included in keys here.
    #[serde(default)]
    pub user_metadata: std::collections::HashMap<String, String>,
    /// `If-Match` header: only succeed if current ETag matches.
    pub if_match: Option<String>,
    /// `If-None-Match` header: typically `"*"` to mean "only if key does not
    /// exist".
    pub if_none_match: Option<String>,
    /// Path to delete after a successful upload. Used internally when the source
    /// file is an encrypted temp copy; not serialized to DB or sent over IPC.
    #[serde(skip)]
    pub cleanup_path: Option<std::path::PathBuf>,
    /// Android SAF staging directory to remove once the upload settles
    /// (Done/Canceled). The whole per-upload dir is deleted, so it survives the
    /// encryption swap that repoints the upload source to a temp file. `None`
    /// for desktop uploads whose source is a real user file. Serialized so a
    /// retry recovers it from the transfer row.
    #[serde(default)]
    pub stage_cleanup_dir: Option<std::path::PathBuf>,
    /// Server-side encryption mode. `None` = provider default; `Some(Sse::S3)`
    /// = SSE-S3 (AES256); `Some(Sse::Kms { key_id })` = SSE-KMS. SSE-C
    /// (customer-provided key) is deliberately not exposed — it requires
    /// secure key transport we don't currently handle.
    pub sse: Option<Sse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Sse {
    /// SSE-S3: AES256 with provider-managed keys.
    S3,
    /// SSE-KMS: KMS-managed key. `key_id` may be `None` for the AWS-managed
    /// default key.
    Kms { key_id: Option<String> },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetOptions {
    pub version_id: Option<String>,
    /// Inclusive start of an HTTP `Range: bytes=` request. `None` plus
    /// `range_end = None` means full object.
    pub range_start: Option<u64>,
    /// Inclusive end of an HTTP `Range: bytes=` request. Setting only
    /// `range_end` requests bytes `0..=range_end`.
    pub range_end: Option<u64>,
    /// Append-mode resume signal: only append to `dest` when this is set by
    /// the transfer manager after confirming a partial file from a prior
    /// attempt exists. Without it, any pre-existing unrelated file at the
    /// destination would silently gain appended bytes.
    #[serde(default)]
    pub resume: bool,
}

/// Outcome of a single `delete_objects` call. `deleted` is the keys the server
/// confirmed gone; `errors` lists per-key failures (typically permissions).
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

/// A single object tag. Keys must match `^[\w +\-=.:/@]{1,128}$` per S3 spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectTag {
    pub key: String,
    pub value: String,
}

/// One CORS rule as stored on a bucket. Mirrors an S3 `CORSRule`. Field names
/// serialize snake_case for the frontend.
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

/// A bucket's full CORS configuration (its list of rules).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CorsConfig {
    pub rules: Vec<CorsRule>,
}

/// Bounded in-memory preview of an object. `truncated = true` when the
/// stored object is larger than the requested cap.
#[derive(Debug, Clone, Serialize)]
pub struct ObjectPreview {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
    pub total_size: Option<i64>,
    pub truncated: bool,
}

/// One in-progress multipart upload as reported by `list_multipart_uploads`.
#[derive(Debug, Clone, Serialize)]
pub struct PendingMultipartUpload {
    pub key: String,
    pub upload_id: String,
    pub initiated_at: Option<i64>,
}

/// The single trait every protocol implementation must provide.
///
/// Method names mirror their S3-equivalents 1:1; non-S3 providers should map
/// their own concepts onto these primitives (e.g. a hypothetical Azure Blob
/// adapter would treat containers as buckets and blobs as objects).
///
/// Streaming methods ([`Self::put_object`], [`Self::get_object`]) take a
/// [`TransferCtx`] carrying a [`tokio_util::sync::CancellationToken`] and a
/// progress sink — these are the cooperative-cancellation + observability
/// hooks used by [`crate::transfer::TransferManager`].
#[async_trait]
pub trait ObjectStore: Send + Sync {
    async fn list_buckets(&self) -> AppResult<Vec<Bucket>>;
    /// Create a bucket. If `region` is `None` the underlying provider uses
    /// the client's default region (typical for non-AWS providers like B2
    /// where a single region is associated with the account).
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
    /// Delete up to 1000 objects in a single request. Returns the list of
    /// keys that the server reported as failed (does *not* error if any
    /// individual delete fails — the caller decides what to do with the
    /// per-key result set).
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
    /// Restore a previous object version by making it the current/latest
    /// version. Implemented as a server-side copy whose source is
    /// `bucket/key?versionId=<version_id>` and whose destination is the same
    /// `bucket/key`; on a versioned bucket this writes a new latest version
    /// whose content equals the chosen old version (also un-deleting an object
    /// whose latest version is a delete marker).
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

    /// Read up to `max_bytes` of an object directly into memory. Intended for
    /// FE previews of small text/image objects. Implementations enforce their
    /// own upper bound; call `read_object_full` when a higher bound is needed
    /// (encrypted-object decrypt path).
    async fn read_object_range(
        &self,
        bucket: &str,
        key: &str,
        max_bytes: u64,
    ) -> AppResult<ObjectPreview>;

    /// Read the entire object into memory, bypassing the preview cap. Used by
    /// the encrypted-preview decrypt path where partial ciphertext cannot be
    /// authenticated by AES-GCM. Callers must enforce their own size guard
    /// via HEAD before invoking this (see `MAX_PREVIEW_DECRYPT_BYTES`).
    async fn read_object_full(&self, bucket: &str, key: &str) -> AppResult<Vec<u8>>;

    /// S3-only feature. Implementations that don't support tagging (Backblaze
    /// B2) should return [`AppError::InvalidInput`] with a clear message so
    /// the FE knows to hide the UI rather than silently doing nothing.
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

    /// Upload raw bytes directly without a local file (used for in-app text editing).
    /// `user_metadata` keys are sent as `x-amz-meta-<key>` (do NOT include the prefix).
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

    /// List in-progress multipart uploads in a bucket. Paginated via
    /// `key_marker` returned in the second tuple slot.
    async fn list_multipart_uploads(
        &self,
        bucket: &str,
        prefix: Option<&str>,
        key_marker: Option<String>,
    ) -> AppResult<(Vec<PendingMultipartUpload>, Option<String>)>;
}
