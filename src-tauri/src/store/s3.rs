//! [`ObjectStore`] implementation backed by the AWS S3 Rust SDK.
//!
//! Works against any S3-compatible service (AWS, Backblaze B2, Cloudflare R2,
//! Wasabi, MinIO, …) — the differentiator is the endpoint URL and addressing
//! style configured on [`S3Config`].
//!
//! Uploads above [`TransferCtx::multipart_threshold`] use S3 multipart with
//! [`TransferCtx::parallelism`] concurrent part uploads and per-part progress
//! emission. Cancellation aborts the in-flight multipart server-side so leaked
//! parts do not accrue cost.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::config::{Region, SharedCredentialsProvider};
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::types::{
    BucketVersioningStatus, CompletedMultipartUpload, CompletedPart, VersioningConfiguration,
};
use aws_sdk_s3::Client;
use aws_smithy_runtime_api::client::result::SdkError;
use aws_smithy_types::error::metadata::ProvideErrorMetadata;
use aws_smithy_types::byte_stream::{ByteStream, Length};
use futures::stream::StreamExt;
use tokio::fs::File;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use crate::error::{AppError, AppResult};
use crate::transfer::{
    CompletedPart as SavedPart, DownloadResult, ResumeState, TransferCtx, TransferEvent,
    UploadResult,
};

use super::{
    Bucket, CannedAcl, CorsConfig, CorsRule, DeleteObjectError, DeleteObjectsResult, GetOptions,
    ListOptions, ListPage, ObjectMeta, ObjectPreview, ObjectStore, ObjectTag, ObjectVersion,
    PendingMultipartUpload, PutOptions, Sse,
};

/// Per-account configuration needed to construct an [`S3Store`].
///
/// `endpoint` is `None` for plain AWS; supply a URL like
/// `https://s3.us-west-004.backblazeb2.com` for non-AWS providers.
///
/// `addressing_style` is one of `"path"`, `"virtual"`, or `"auto"`. Auto enables
/// path-style whenever a custom endpoint is set (matches B2 / R2 / MinIO).
#[derive(Debug, Clone)]
pub struct S3Config {
    pub region: String,
    pub endpoint: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub addressing_style: String,
}

/// S3 client wrapper. Internally cheap to clone — wraps the SDK's `Client`,
/// which itself is `Arc`-shared.
#[derive(Clone)]
pub struct S3Store {
    client: Client,
}

impl S3Store {
    pub async fn new(cfg: S3Config) -> AppResult<Self> {
        let creds = Credentials::new(
            cfg.access_key_id,
            cfg.secret_access_key,
            None,
            None,
            "cosmog-static",
        );

        let mut builder = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(cfg.region))
            .credentials_provider(SharedCredentialsProvider::new(creds));

        if let Some(endpoint) = cfg.endpoint.as_deref() {
            builder = builder.endpoint_url(endpoint);
        }

        let force_path = match cfg.addressing_style.as_str() {
            "path" => true,
            "virtual" => false,
            _ => cfg.endpoint.is_some(),
        };
        builder = builder.force_path_style(force_path);

        #[cfg(target_os = "android")]
        {
            builder = builder.http_client(crate::store::android_tls::http_client());
        }

        let client = Client::from_conf(builder.build());
        Ok(S3Store { client })
    }
}

/// Map an arbitrary error into [`AppError::S3`] with a context tag. Used for
/// non-SDK failures (e.g. building a `ByteStream`) where no S3 error code is
/// available.
fn s3_err<E: std::fmt::Display>(ctx: &str, e: E) -> AppError {
    AppError::S3(format!("{ctx}: {e}"))
}

/// Flatten an error and its `source()` chain into one readable string. The AWS
/// SDK hides the real network cause (DNS, refused, TLS) under a generic outer
/// Display, so surfacing the chain makes dispatch failures actionable.
fn error_chain<E: std::error::Error>(e: &E) -> String {
    let mut parts = vec![e.to_string()];
    let mut src = e.source();
    while let Some(s) = src {
        let msg = s.to_string();
        if !parts.contains(&msg) {
            parts.push(msg);
        }
        src = s.source();
    }
    parts.join(": ")
}

/// Classify an AWS SDK error into the most specific [`AppError`] variant
/// available. Falls back to [`AppError::S3`] for unknown codes or non-service
/// failures (timeouts, DNS, etc.).
fn classify_aws<E>(
    ctx: &str,
    err: SdkError<E, aws_smithy_runtime_api::client::orchestrator::HttpResponse>,
) -> AppError
where
    E: ProvideErrorMetadata + std::fmt::Display + std::fmt::Debug,
{
    let mut display = format!("{ctx}: {err}");

    // Network-level failures: connection refused, DNS failure, TLS, TCP
    // timeout. These fire before any HTTP response exists, so no service error
    // code. The SDK's outer Display collapses to a bare "dispatch failure", so
    // walk the source chain to surface the real cause (DNS, refused, TLS, ...).
    if let SdkError::DispatchFailure(ref df) = err {
        let msg = if df.is_timeout() {
            format!("{ctx}: connection timed out")
        } else if let Some(ce) = df.as_connector_error() {
            format!("{ctx}: {}", error_chain(ce))
        } else {
            display.clone()
        };
        return AppError::NetworkUnreachable(msg);
    }
    if let SdkError::TimeoutError(_) = err {
        return AppError::NetworkUnreachable(format!("{ctx}: request timed out"));
    }

    if let Some(service_err) = err.as_service_error() {
        let code = service_err.code().unwrap_or_default();
        let msg = service_err.message().unwrap_or_default();
        // The SDK's outer Display often collapses to "service error" — surface
        // the code + message so logs and UI errors are actionable.
        if !code.is_empty() || !msg.is_empty() {
            display = format!("{ctx}: {code} {msg}").trim().to_string();
        }
        match code {
            "NoSuchBucket" | "NoSuchKey" | "NoSuchUpload" | "NoSuchVersion"
            | "NotFound" | "404" => {
                return AppError::NotFound(display);
            }
            // Bad-credential codes get a dedicated variant so UI can prompt
            // for re-entry rather than just toasting "access denied".
            "SignatureDoesNotMatch" | "InvalidAccessKeyId" => {
                return AppError::CredentialsInvalid(display);
            }
            "AccessDenied" | "AllAccessDisabled" | "UnauthorizedAccess" => {
                return AppError::AccessDenied(display);
            }
            "PreconditionFailed"
            | "BucketAlreadyExists"
            | "BucketAlreadyOwnedByYou"
            | "BucketNotEmpty"
            | "OperationAborted" => {
                return AppError::Conflict(display);
            }
            "SlowDown" | "TooManyRequests" | "RequestTimeTooSkewed" => {
                return AppError::RateLimited(display);
            }
            "PermanentRedirect" => {
                return AppError::RegionRedirect(display);
            }
            // Provider doesn't implement this operation (common on B2/R2 for
            // bucket policy or CORS). Give the FE a distinct code to switch on.
            "NotImplemented" | "501" => {
                return AppError::Unsupported(display);
            }
            // Object's storage class blocks direct reads (archive/cold tier);
            // GetObject fails until restored. Distinct code so FE can explain.
            "InvalidObjectState" => {
                return AppError::Archived(display);
            }
            _ => {}
        }
        // Some providers signal "not implemented" only via HTTP 501 with an
        // empty/unknown error code. Classify from the raw status.
        if let SdkError::ServiceError(ref se) = err {
            if se.raw().status().as_u16() == 501 {
                return AppError::Unsupported(display);
            }
        }
    }
    // HEAD responses (HeadBucket/HeadObject) carry no XML error body, so a
    // cross-region 301 yields an empty error code above. Classify it from the
    // raw HTTP status so region auto-recovery still kicks in.
    if let SdkError::ServiceError(ref se) = err {
        if se.raw().status().as_u16() == 301 {
            return AppError::RegionRedirect(display);
        }
    }
    AppError::S3(display)
}

fn canceled(transfer_id: &str) -> AppError {
    AppError::Canceled(format!("transfer {transfer_id} canceled"))
}

/// True when saved multipart resume state was built from a different version
/// of the source file than the one currently on disk (size or mtime changed).
/// Unknown fingerprints — rows persisted before the fields existed, or a stat
/// failure at enqueue — never count as stale: resuming is then the same
/// best-effort guess it always was.
fn resume_state_is_stale(state: &ResumeState, ctx: &TransferCtx) -> bool {
    match (state.source_len, state.source_mtime_secs, ctx.source_stat) {
        (Some(len), Some(mtime), Some(cur)) => len != cur.len || mtime != cur.mtime_secs,
        _ => false,
    }
}

/// True when a ranged GetObject was rejected *because of the range itself* —
/// HTTP 416 or the provider's `InvalidRange` / `RangeNotSatisfiable` error
/// code (e.g. requesting bytes of a 0-byte object, providers without Range
/// support). This is the only failure class where falling back to a full-
/// object GET is meaningful.
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

/// Parse the `total` portion of an HTTP `Content-Range: bytes a-b/total`
/// response. Returns `None` for `*` (unknown total) or malformed input.
fn parse_content_range_total(header: &str) -> Option<i64> {
    let after_slash = header.rsplit_once('/')?.1.trim();
    if after_slash == "*" {
        return None;
    }
    after_slash.parse::<i64>().ok()
}

/// Format an HTTP `Range: bytes=` value from optional start/end. Returns
/// `None` when both are `None` (meaning "no range header — full object").
fn build_range_header(start: Option<u64>, end: Option<u64>) -> Option<String> {
    match (start, end) {
        (None, None) => None,
        (Some(s), Some(e)) => Some(format!("bytes={s}-{e}")),
        (Some(s), None) => Some(format!("bytes={s}-")),
        (None, Some(e)) => Some(format!("bytes=0-{e}")),
    }
}

/// Emit progress, but throttle so we don't flood the channel.
///
/// Uses an AtomicU64 storing nanoseconds from an arbitrary epoch so that
/// `allow()` is lock-free and can be called from every chunk callback without
/// contention.
struct ProgressThrottle {
    /// Nanoseconds since the process started at which we last emitted.
    last_ns: std::sync::atomic::AtomicU64,
    interval_ns: u64,
    /// Anchor point so we can compute nanoseconds cheaply.
    epoch: Instant,
}

impl ProgressThrottle {
    fn new(interval_ms: u64) -> Self {
        let interval_ns = interval_ms * 1_000_000;
        Self {
            // Pretend the last emit happened in the past so the first allow() fires.
            // Use 0 as a sentinel meaning "never fired"; allow() treats it specially.
            last_ns: std::sync::atomic::AtomicU64::new(0),
            interval_ns,
            epoch: Instant::now(),
        }
    }

    fn allow(&self) -> bool {
        use std::sync::atomic::Ordering;
        // Ensure now_ns is never 0 — we use 0 as "never fired" sentinel.
        let now_ns = (self.epoch.elapsed().as_nanos() as u64).max(1);
        let last = self.last_ns.load(Ordering::Relaxed);
        // First call (last==0) always fires; subsequent calls obey the interval.
        if last == 0 || now_ns.saturating_sub(last) >= self.interval_ns {
            // CAS: only one concurrent caller wins; others skip this tick.
            self.last_ns.compare_exchange(last, now_ns, Ordering::Relaxed, Ordering::Relaxed).is_ok()
        } else {
            false
        }
    }
}

#[async_trait]
impl ObjectStore for S3Store {
    async fn list_buckets(&self) -> AppResult<Vec<Bucket>> {
        // Paginate instead of a single send(): accounts with many buckets
        // only ever see one 1000-bucket page otherwise.
        const MAX_BUCKETS: usize = 10_000;
        let mut out = Vec::new();
        let mut pages = self.client.list_buckets().into_paginator().send();
        while let Some(page) = pages.next().await {
            let page = page.map_err(|e| classify_aws("list_buckets", e))?;
            for b in page.buckets() {
                if out.len() >= MAX_BUCKETS {
                    tracing::warn!(
                        "list_buckets returned more than {MAX_BUCKETS} buckets; truncating"
                    );
                    return Ok(out);
                }
                out.push(Bucket {
                    name: b.name().unwrap_or_default().to_string(),
                    created_at: b.creation_date().map(|d| d.secs()),
                });
            }
        }
        Ok(out)
    }

    async fn create_bucket(&self, name: &str, region: Option<&str>) -> AppResult<()> {
        use aws_sdk_s3::types::{BucketLocationConstraint, CreateBucketConfiguration};
        let mut req = self.client.create_bucket().bucket(name);
        if let Some(r) = region {
            // us-east-1 is implicit in the S3 protocol — sending a
            // LocationConstraint of "us-east-1" is in fact illegal. Skip it.
            if r != "us-east-1" {
                let cfg = CreateBucketConfiguration::builder()
                    .location_constraint(BucketLocationConstraint::from(r))
                    .build();
                req = req.create_bucket_configuration(cfg);
            }
        }
        req.send()
            .await
            .map_err(|e| classify_aws("create_bucket", e))?;
        Ok(())
    }

    async fn delete_bucket(&self, name: &str) -> AppResult<()> {
        self.client
            .delete_bucket()
            .bucket(name)
            .send()
            .await
            .map_err(|e| classify_aws("delete_bucket", e))?;
        Ok(())
    }

    async fn head_bucket(&self, name: &str) -> AppResult<()> {
        self.client
            .head_bucket()
            .bucket(name)
            .send()
            .await
            .map_err(|e| classify_aws("head_bucket", e))?;
        Ok(())
    }

    async fn get_bucket_location(&self, name: &str) -> AppResult<Option<String>> {
        let resp = self
            .client
            .get_bucket_location()
            .bucket(name)
            .send()
            .await
            .map_err(|e| classify_aws("get_bucket_location", e))?;
        // AWS returns an empty LocationConstraint for us-east-1 (SDK quirk).
        // Normalise to None so callers can use unwrap_or("us-east-1").
        Ok(resp
            .location_constraint()
            .map(|c| c.as_str().to_string())
            .filter(|s| !s.is_empty()))
    }

    async fn put_bucket_acl(&self, name: &str, acl: CannedAcl) -> AppResult<()> {
        use aws_sdk_s3::types::BucketCannedAcl;
        let v = match acl {
            CannedAcl::Private => BucketCannedAcl::Private,
            CannedAcl::PublicRead => BucketCannedAcl::PublicRead,
        };
        self.client
            .put_bucket_acl()
            .bucket(name)
            .acl(v)
            .send()
            .await
            .map_err(|e| classify_aws("put_bucket_acl", e))?;
        Ok(())
    }

    async fn get_bucket_versioning(&self, name: &str) -> AppResult<bool> {
        let resp = self
            .client
            .get_bucket_versioning()
            .bucket(name)
            .send()
            .await
            .map_err(|e| classify_aws("get_bucket_versioning", e))?;
        Ok(matches!(resp.status(), Some(BucketVersioningStatus::Enabled)))
    }

    async fn put_bucket_versioning(&self, name: &str, enabled: bool) -> AppResult<()> {
        let status = if enabled {
            BucketVersioningStatus::Enabled
        } else {
            BucketVersioningStatus::Suspended
        };
        let cfg = VersioningConfiguration::builder().status(status).build();
        self.client
            .put_bucket_versioning()
            .bucket(name)
            .versioning_configuration(cfg)
            .send()
            .await
            .map_err(|e| classify_aws("put_bucket_versioning", e))?;
        Ok(())
    }

    async fn get_bucket_policy(&self, name: &str) -> AppResult<Option<String>> {
        let resp = self
            .client
            .get_bucket_policy()
            .bucket(name)
            .send()
            .await;
        match resp {
            Ok(r) => Ok(r.policy().map(|s| s.to_string())),
            Err(e) => {
                // No policy on the bucket is a normal "absent" state, not an error.
                if e.as_service_error()
                    .and_then(|se| se.code())
                    .map(|c| c.contains("NoSuchBucketPolicy"))
                    .unwrap_or(false)
                {
                    return Ok(None);
                }
                Err(classify_aws("get_bucket_policy", e))
            }
        }
    }

    async fn put_bucket_policy(&self, name: &str, policy: String) -> AppResult<()> {
        self.client
            .put_bucket_policy()
            .bucket(name)
            .policy(policy)
            .send()
            .await
            .map_err(|e| classify_aws("put_bucket_policy", e))?;
        Ok(())
    }

    async fn delete_bucket_policy(&self, name: &str) -> AppResult<()> {
        let resp = self
            .client
            .delete_bucket_policy()
            .bucket(name)
            .send()
            .await;
        match resp {
            Ok(_) => Ok(()),
            Err(e) => {
                // Deleting an absent policy is a no-op success.
                if e.as_service_error()
                    .and_then(|se| se.code())
                    .map(|c| c.contains("NoSuchBucketPolicy"))
                    .unwrap_or(false)
                {
                    return Ok(());
                }
                Err(classify_aws("delete_bucket_policy", e))
            }
        }
    }

    async fn get_bucket_cors(&self, name: &str) -> AppResult<Option<CorsConfig>> {
        let resp = self.client.get_bucket_cors().bucket(name).send().await;
        match resp {
            Ok(r) => {
                let rules = r
                    .cors_rules()
                    .iter()
                    .map(|rule| CorsRule {
                        id: rule.id().map(|s| s.to_string()),
                        allowed_origins: rule
                            .allowed_origins()
                            .iter()
                            .map(|s| s.to_string())
                            .collect(),
                        allowed_methods: rule
                            .allowed_methods()
                            .iter()
                            .map(|s| s.to_string())
                            .collect(),
                        allowed_headers: rule
                            .allowed_headers()
                            .iter()
                            .map(|s| s.to_string())
                            .collect(),
                        expose_headers: rule
                            .expose_headers()
                            .iter()
                            .map(|s| s.to_string())
                            .collect(),
                        max_age_seconds: rule.max_age_seconds(),
                    })
                    .collect();
                Ok(Some(CorsConfig { rules }))
            }
            Err(e) => {
                if e.as_service_error()
                    .and_then(|se| se.code())
                    .map(|c| c.contains("NoSuchCORSConfiguration"))
                    .unwrap_or(false)
                {
                    return Ok(None);
                }
                Err(classify_aws("get_bucket_cors", e))
            }
        }
    }

    async fn put_bucket_cors(&self, name: &str, cors: CorsConfig) -> AppResult<()> {
        use aws_sdk_s3::types::{CorsConfiguration, CorsRule as AwsCorsRule};
        let rules: Vec<AwsCorsRule> = cors
            .rules
            .into_iter()
            .map(|r| {
                AwsCorsRule::builder()
                    .set_id(r.id)
                    .set_allowed_origins(Some(r.allowed_origins))
                    .set_allowed_methods(Some(r.allowed_methods))
                    .set_allowed_headers(Some(r.allowed_headers))
                    .set_expose_headers(Some(r.expose_headers))
                    .set_max_age_seconds(r.max_age_seconds)
                    .build()
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| s3_err("put_bucket_cors build", e))?;
        let cfg = CorsConfiguration::builder()
            .set_cors_rules(Some(rules))
            .build()
            .map_err(|e| s3_err("put_bucket_cors build", e))?;
        self.client
            .put_bucket_cors()
            .bucket(name)
            .cors_configuration(cfg)
            .send()
            .await
            .map_err(|e| classify_aws("put_bucket_cors", e))?;
        Ok(())
    }

    async fn delete_bucket_cors(&self, name: &str) -> AppResult<()> {
        let resp = self.client.delete_bucket_cors().bucket(name).send().await;
        match resp {
            Ok(_) => Ok(()),
            Err(e) => {
                if e.as_service_error()
                    .and_then(|se| se.code())
                    .map(|c| c.contains("NoSuchCORSConfiguration"))
                    .unwrap_or(false)
                {
                    return Ok(());
                }
                Err(classify_aws("delete_bucket_cors", e))
            }
        }
    }

    async fn list_objects(&self, bucket: &str, opts: ListOptions) -> AppResult<ListPage> {
        let mut req = self.client.list_objects_v2().bucket(bucket);
        if let Some(p) = opts.prefix {
            req = req.prefix(p);
        }
        if let Some(d) = opts.delimiter {
            req = req.delimiter(d);
        }
        if let Some(c) = opts.continuation {
            req = req.continuation_token(c);
        }
        if let Some(m) = opts.max_keys {
            req = req.max_keys(m);
        }
        let resp = req.send().await.map_err(|e| classify_aws("list_objects", e))?;

        let objects = resp
            .contents()
            .iter()
            .map(|o| ObjectMeta {
                key: o.key().unwrap_or_default().to_string(),
                size: o.size().unwrap_or_default(),
                etag: o.e_tag().map(|s| s.to_string()),
                last_modified: o.last_modified().map(|d| d.secs()),
                storage_class: o.storage_class().map(|s| s.as_str().to_string()),
                content_type: None,
                version_id: None,
                user_metadata: Default::default(),
            })
            .collect();

        let prefixes = resp
            .common_prefixes()
            .iter()
            .filter_map(|p| p.prefix().map(|s| s.to_string()))
            .collect();

        Ok(ListPage {
            objects,
            prefixes,
            continuation: resp.next_continuation_token().map(|s| s.to_string()),
            is_truncated: resp.is_truncated().unwrap_or(false),
        })
    }

    async fn head_object(&self, bucket: &str, key: &str) -> AppResult<ObjectMeta> {
        let resp = self
            .client
            .head_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| classify_aws("head_object", e))?;
        // Copy user metadata off HEAD. Keys are already stripped of the
        // `x-amz-meta-` prefix by the AWS SDK.
        let user_metadata = resp
            .metadata()
            .map(|m| m.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
            .unwrap_or_default();
        Ok(ObjectMeta {
            key: key.to_string(),
            size: resp.content_length().unwrap_or_default(),
            etag: resp.e_tag().map(|s| s.to_string()),
            last_modified: resp.last_modified().map(|d| d.secs()),
            storage_class: resp.storage_class().map(|s| s.as_str().to_string()),
            content_type: resp.content_type().map(|s| s.to_string()),
            version_id: resp.version_id().map(|s| s.to_string()),
            user_metadata,
        })
    }

    async fn put_object_bytes(
        &self,
        bucket: &str,
        key: &str,
        content_type: &str,
        data: Vec<u8>,
        user_metadata: std::collections::HashMap<String, String>,
    ) -> AppResult<()> {
        let len = data.len() as i64;
        let mut req = self.client
            .put_object()
            .bucket(bucket)
            .key(key)
            .content_type(content_type)
            .content_length(len);
        for (k, v) in user_metadata {
            req = req.metadata(k, v);
        }
        req
            .body(ByteStream::from(data))
            .send()
            .await
            .map_err(|e| classify_aws("put_object_bytes", e))?;
        Ok(())
    }

    async fn create_folder(&self, bucket: &str, prefix: &str) -> AppResult<()> {
        let key = format!("{}/", prefix.trim_end_matches('/'));
        self.client
            .put_object()
            .bucket(bucket)
            .key(&key)
            .content_type("application/x-directory")
            .content_length(0)
            .body(ByteStream::from_static(b""))
            .send()
            .await
            .map_err(|e| classify_aws("create_folder", e))?;
        Ok(())
    }

    async fn delete_object(&self, bucket: &str, key: &str) -> AppResult<()> {
        self.client
            .delete_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| classify_aws("delete_object", e))?;
        Ok(())
    }

    async fn delete_objects(
        &self,
        bucket: &str,
        keys: &[String],
    ) -> AppResult<DeleteObjectsResult> {
        use aws_sdk_s3::types::{Delete, ObjectIdentifier};
        if keys.is_empty() {
            return Ok(DeleteObjectsResult {
                deleted: Vec::new(),
                errors: Vec::new(),
            });
        }
        if keys.len() > 1000 {
            return Err(AppError::InvalidInput(
                "delete_objects accepts at most 1000 keys per call".into(),
            ));
        }
        let identifiers: Vec<ObjectIdentifier> = keys
            .iter()
            .map(|k| ObjectIdentifier::builder().key(k).build())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| s3_err("delete_objects build", e))?;
        let delete = Delete::builder()
            .set_objects(Some(identifiers))
            .quiet(false)
            .build()
            .map_err(|e| s3_err("delete_objects build", e))?;
        let resp = self
            .client
            .delete_objects()
            .bucket(bucket)
            .delete(delete)
            .send()
            .await
            .map_err(|e| classify_aws("delete_objects", e))?;
        let deleted: Vec<String> = resp
            .deleted()
            .iter()
            .filter_map(|d| d.key().map(|s| s.to_string()))
            .collect();
        let errors: Vec<DeleteObjectError> = resp
            .errors()
            .iter()
            .map(|e| DeleteObjectError {
                key: e.key().unwrap_or_default().to_string(),
                code: e.code().map(|s| s.to_string()),
                message: e.message().map(|s| s.to_string()),
            })
            .collect();
        Ok(DeleteObjectsResult { deleted, errors })
    }

    async fn delete_object_version(
        &self,
        bucket: &str,
        key: &str,
        version_id: &str,
    ) -> AppResult<()> {
        self.client
            .delete_object()
            .bucket(bucket)
            .key(key)
            .version_id(version_id)
            .send()
            .await
            .map_err(|e| classify_aws("delete_object_version", e))?;
        Ok(())
    }

    async fn restore_object_version(
        &self,
        bucket: &str,
        key: &str,
        version_id: &str,
    ) -> AppResult<()> {
        // Restore = server-side CopyObject where the source is the chosen old
        // version (`bucket/key?versionId=...`) and the destination is the same
        // `bucket/key`. On a versioned bucket this creates a new latest version
        // whose bytes equal the old version. Callers must not point this at a
        // delete marker (S3 rejects copying from one); un-deleting is done by
        // deleting the marker instead.
        //
        // CopySource requires each key path segment to be percent-encoded
        // (keys can contain #, ?, spaces, non-ASCII). Slashes are path
        // separators and must not be encoded. This mirrors `copy_object`.
        let encoded_key: String = key
            .split('/')
            .map(|seg| urlencoding::encode(seg).into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let copy_source = format!("{bucket}/{encoded_key}?versionId={version_id}");
        self.client
            .copy_object()
            .copy_source(copy_source)
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| classify_aws("restore_object_version", e))?;
        Ok(())
    }

    async fn list_object_versions(
        &self,
        bucket: &str,
        prefix: Option<&str>,
        continuation: Option<String>,
    ) -> AppResult<(Vec<ObjectVersion>, Option<String>)> {
        // ListObjectVersions pages within a single key using BOTH a key-marker
        // and a version-id-marker: an object with more versions than fit in one
        // response is only fully listed if we echo both on the follow-up. We
        // carry the pair as a JSON continuation token so the frontend can round
        // -trip it opaquely.
        #[derive(serde::Serialize, serde::Deserialize)]
        struct VersionsMarker {
            key: String,
            version_id: Option<String>,
        }

        let mut req = self.client.list_object_versions().bucket(bucket);
        if let Some(p) = prefix {
            req = req.prefix(p);
        }
        if let Some(token) = continuation {
            if let Ok(m) = serde_json::from_str::<VersionsMarker>(&token) {
                req = req.key_marker(m.key);
                if let Some(vid) = m.version_id {
                    req = req.version_id_marker(vid);
                }
            }
        }
        let resp = req
            .send()
            .await
            .map_err(|e| classify_aws("list_object_versions", e))?;

        let mut out: Vec<ObjectVersion> = resp
            .versions()
            .iter()
            .map(|v| ObjectVersion {
                key: v.key().unwrap_or_default().to_string(),
                version_id: v.version_id().map(|s| s.to_string()),
                is_latest: v.is_latest().unwrap_or(false),
                is_delete_marker: false,
                size: Some(v.size().unwrap_or_default()),
                etag: v.e_tag().map(|s| s.to_string()),
                last_modified: v.last_modified().map(|d| d.secs()),
            })
            .collect();

        out.extend(resp.delete_markers().iter().map(|d| ObjectVersion {
            key: d.key().unwrap_or_default().to_string(),
            version_id: d.version_id().map(|s| s.to_string()),
            is_latest: d.is_latest().unwrap_or(false),
            is_delete_marker: true,
            size: None,
            etag: None,
            last_modified: d.last_modified().map(|x| x.secs()),
        }));

        // Only continue while S3 reports truncation, echoing both markers.
        let next = if resp.is_truncated().unwrap_or(false) {
            resp.next_key_marker().map(|k| {
                let m = VersionsMarker {
                    key: k.to_string(),
                    version_id: resp.next_version_id_marker().map(|s| s.to_string()),
                };
                serde_json::to_string(&m).unwrap_or_default()
            })
        } else {
            None
        };
        Ok((out, next))
    }

    async fn copy_object(
        &self,
        src_bucket: &str,
        src_key: &str,
        dst_bucket: &str,
        dst_key: &str,
    ) -> AppResult<()> {
        // CopySource requires each key path segment to be percent-encoded
        // (keys can contain #, ?, spaces, non-ASCII).  Slashes are path
        // separators and must not be encoded.
        let encoded_key: String = src_key
            .split('/')
            .map(|seg| urlencoding::encode(seg).into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let copy_source = format!("{src_bucket}/{encoded_key}");
        self.client
            .copy_object()
            .copy_source(copy_source)
            .bucket(dst_bucket)
            .key(dst_key)
            .send()
            .await
            .map_err(|e| classify_aws("copy_object", e))?;
        Ok(())
    }

    async fn put_object_acl(&self, bucket: &str, key: &str, acl: CannedAcl) -> AppResult<()> {
        use aws_sdk_s3::types::ObjectCannedAcl;
        let v = match acl {
            CannedAcl::Private => ObjectCannedAcl::Private,
            CannedAcl::PublicRead => ObjectCannedAcl::PublicRead,
        };
        self.client
            .put_object_acl()
            .bucket(bucket)
            .key(key)
            .acl(v)
            .send()
            .await
            .map_err(|e| classify_aws("put_object_acl", e))?;
        Ok(())
    }

    async fn presign_get(&self, bucket: &str, key: &str, expires_secs: u64) -> AppResult<String> {
        let cfg = PresigningConfig::expires_in(Duration::from_secs(expires_secs))
            .map_err(|e| s3_err("presign_config", e))?;
        let presigned = self
            .client
            .get_object()
            .bucket(bucket)
            .key(key)
            .presigned(cfg)
            .await
            .map_err(|e| classify_aws("presign_get", e))?;
        Ok(presigned.uri().to_string())
    }

    async fn get_object_tagging(
        &self,
        bucket: &str,
        key: &str,
    ) -> AppResult<Vec<ObjectTag>> {
        let resp = self
            .client
            .get_object_tagging()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| classify_aws("get_object_tagging", e))?;
        Ok(resp
            .tag_set()
            .iter()
            .map(|t| ObjectTag {
                key: t.key().to_string(),
                value: t.value().to_string(),
            })
            .collect())
    }

    async fn put_object_tagging(
        &self,
        bucket: &str,
        key: &str,
        tags: &[ObjectTag],
    ) -> AppResult<()> {
        use aws_sdk_s3::types::{Tag, Tagging};
        let tag_set: Vec<Tag> = tags
            .iter()
            .map(|t| Tag::builder().key(&t.key).value(&t.value).build())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| s3_err("put_object_tagging build", e))?;
        let tagging = Tagging::builder()
            .set_tag_set(Some(tag_set))
            .build()
            .map_err(|e| s3_err("put_object_tagging build", e))?;
        self.client
            .put_object_tagging()
            .bucket(bucket)
            .key(key)
            .tagging(tagging)
            .send()
            .await
            .map_err(|e| classify_aws("put_object_tagging", e))?;
        Ok(())
    }

    async fn delete_object_tagging(&self, bucket: &str, key: &str) -> AppResult<()> {
        self.client
            .delete_object_tagging()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| classify_aws("delete_object_tagging", e))?;
        Ok(())
    }

    async fn read_object_range(
        &self,
        bucket: &str,
        key: &str,
        max_bytes: u64,
    ) -> AppResult<ObjectPreview> {
        const HARD_CAP: u64 = 8 * 1024 * 1024;
        let cap = max_bytes.min(HARD_CAP);

        // Try ranged GET. Fall back to a full GET only when the provider
        // rejects ranges outright (HTTP 416 / InvalidRange class — e.g.
        // 0-byte objects, providers without Range support). Any other error
        // (404, credentials, network, …) must surface as-is: masking it with
        // a fallback GET used to turn real failures into confusing
        // whole-object downloads.
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
            // Preserve the original ranged-GET error with its own context.
            Err(e) => return Err(classify_aws("get_object preview range", e)),
        };

        // For ranged GET the SDK's content_length reflects the range payload,
        // not the object. Parse Content-Range "bytes a-b/total" for the real size.
        let total_from_range = if used_range {
            resp.content_range().and_then(parse_content_range_total)
        } else {
            None
        };
        let payload_len = resp.content_length();
        // Guard RAM before collecting: refuse to buffer more than `cap` even
        // if Content-Length lies or is absent — read at most `cap` bytes and
        // mark the preview truncated.
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

    async fn read_object_full(&self, bucket: &str, key: &str) -> AppResult<Vec<u8>> {
        let resp = self
            .client
            .get_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| classify_aws("get_object", e))?;
        let body = resp
            .body
            .collect()
            .await
            .map_err(|e| s3_err("get_object full body", e))?;
        Ok(body.to_vec())
    }

    async fn abort_multipart_upload(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
    ) -> AppResult<()> {
        self.client
            .abort_multipart_upload()
            .bucket(bucket)
            .key(key)
            .upload_id(upload_id)
            .send()
            .await
            .map_err(|e| classify_aws("abort_multipart_upload", e))?;
        Ok(())
    }

    async fn list_multipart_uploads(
        &self,
        bucket: &str,
        prefix: Option<&str>,
        key_marker: Option<String>,
    ) -> AppResult<(Vec<PendingMultipartUpload>, Option<String>)> {
        let mut req = self.client.list_multipart_uploads().bucket(bucket);
        if let Some(p) = prefix {
            req = req.prefix(p);
        }
        if let Some(km) = key_marker {
            req = req.key_marker(km);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| classify_aws("list_multipart_uploads", e))?;
        let uploads = resp
            .uploads()
            .iter()
            .filter_map(|u| {
                let key = u.key()?.to_string();
                let upload_id = u.upload_id()?.to_string();
                Some(PendingMultipartUpload {
                    key,
                    upload_id,
                    initiated_at: u.initiated().map(|d| d.secs()),
                })
            })
            .collect();
        let next = if resp.is_truncated().unwrap_or(false) {
            resp.next_key_marker().map(|s| s.to_string())
        } else {
            None
        };
        Ok((uploads, next))
    }

    async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        source: PathBuf,
        opts: PutOptions,
        ctx: TransferCtx,
    ) -> AppResult<UploadResult> {
        let meta = tokio::fs::metadata(&source).await?;
        let total = meta.len();

        ctx.progress.emit(TransferEvent::Started {
            transfer_id: ctx.transfer_id.clone(),
            bytes_total: Some(total),
        });

        if total <= ctx.multipart_threshold && ctx.resume.is_none() {
            return self
                .put_single(bucket, key, source, total, opts, ctx)
                .await;
        }

        self.put_multipart(bucket, key, source, total, opts, ctx).await
    }

    async fn get_object(
        &self,
        bucket: &str,
        key: &str,
        dest: PathBuf,
        opts: GetOptions,
        ctx: TransferCtx,
    ) -> AppResult<DownloadResult> {
        // Honour a pre-cancelled token before issuing any network request.
        if ctx.cancel.is_cancelled() {
            ctx.progress.emit(TransferEvent::Canceled {
                transfer_id: ctx.transfer_id.clone(),
            });
            return Err(canceled(&ctx.transfer_id));
        }
        // For full-object downloads, attempt a HEAD to get size and decide
        // whether to use parallel chunked GETs.
        if opts.range_start.is_none() && opts.range_end.is_none() {
            if let Ok(head) = self.client.head_object().bucket(bucket).key(key).send().await {
                if let Some(total) = head.content_length().map(|n| n as u64) {
                    if total > ctx.multipart_threshold {
                        return self
                            .get_object_parallel(bucket, key, &dest, total, &opts, &ctx)
                            .await;
                    }
                }
            }
            // HEAD failed or size <= threshold — fall through to single-stream GET.
        }

        let mut req = self.client.get_object().bucket(bucket).key(key);
        if let Some(v) = opts.version_id {
            req = req.version_id(v);
        }
        if let Some(range) = build_range_header(opts.range_start, opts.range_end) {
            req = req.range(range);
        }
        let resp = req.send().await.map_err(|e| classify_aws("get_object", e))?;

        // For a ranged GET, `content_length` is the *range* size, not the
        // whole object; the `Content-Range` header carries the real total.
        // Fall back to content_length for full-object GETs.
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
        // Only append when the caller explicitly signalled resume (the
        // transfer manager sets this after confirming a partial file from a
        // prior attempt exists). Inferring resume from `dest.exists()` used
        // to append range payloads onto unrelated pre-existing files.
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
                    // Delete only what we created/truncated ourselves. While
                    // resuming onto a pre-existing partial file, leave it
                    // intact so a later retry can continue from the same
                    // offset instead of starting over.
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

        Ok(DownloadResult { bytes: bytes_done })
    }
}

impl S3Store {
    /// Download a large object by issuing parallel range GETs and writing each
    /// chunk into the correct offset of a pre-allocated file.
    async fn get_object_parallel(
        &self,
        bucket: &str,
        key: &str,
        dest: &PathBuf,
        total: u64,
        opts: &GetOptions,
        ctx: &TransferCtx,
    ) -> AppResult<DownloadResult> {
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        {
            let file = File::create(dest).await?;
            file.set_len(total).await?;
        }

        let part_size = ctx.part_size.max(5 * 1024 * 1024);
        let num_parts = (total + part_size - 1) / part_size;

        let client = self.client.clone();
        let bucket_s = bucket.to_string();
        let key_s = key.to_string();
        let version_id = opts.version_id.clone();
        let dest_s = dest.clone();
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

                // Stream the part body straight to the file. Open once, seek to
                // this part's offset once, then write chunk-by-chunk so resident
                // memory is one chunk (~256 KiB) not the whole part (part_size).
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
                let _ = tokio::fs::remove_file(dest).await;
                let event = if matches!(&e, AppError::Canceled(_)) {
                    TransferEvent::Canceled { transfer_id: ctx.transfer_id.clone() }
                } else {
                    TransferEvent::Failed { transfer_id: ctx.transfer_id.clone(), error: e.to_string() }
                };
                ctx.progress.emit(event);
                return Err(e);
            }
        }

        ctx.progress.emit(TransferEvent::Progress {
            transfer_id: ctx.transfer_id.clone(),
            bytes_done: total,
            bytes_total: Some(total),
        });
        ctx.progress.emit(TransferEvent::Done {
            transfer_id: ctx.transfer_id.clone(),
            etag: None,
        });

        Ok(DownloadResult { bytes: total })
    }

    async fn put_single(
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

    /// Upload a file as a multipart S3 upload. Concurrency, part size, and
    /// resume state come from `ctx`. Aborts the upload server-side on error or
    /// cancellation.
    async fn put_multipart(
        &self,
        bucket: &str,
        key: &str,
        source: PathBuf,
        total: u64,
        opts: PutOptions,
        ctx: TransferCtx,
    ) -> AppResult<UploadResult> {
        let (upload_id, already_done) = self.init_or_resume_upload(bucket, key, &opts, &ctx).await?;
        // Publish the upload_id immediately so the worker can resume this same
        // upload on a retry, or abort it on final give-up, even if zero parts
        // complete before an error.
        ctx.progress.emit(TransferEvent::MultipartInitiated {
            transfer_id: ctx.transfer_id.clone(),
            upload_id: upload_id.clone(),
        });

        let part_size = ctx.part_size.max(5 * 1024 * 1024); // S3 floor for non-final parts
        let num_parts_u64 = (total + part_size - 1) / part_size;
        if num_parts_u64 > 10_000 {
            return Err(crate::error::AppError::InvalidInput(format!(
                "file too large: would require {num_parts_u64} parts (S3 limit is 10,000); increase part size"
            )));
        }
        let num_parts = num_parts_u64 as i32;
        let bytes_done = Arc::new(AtomicU64::new(
            // Compute the precise number of bytes already uploaded by walking
            // the part numbers in `already_done` and summing each part's
            // actual length (final part is shorter than `part_size`).
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
                    // Do NOT abort or emit a terminal event here: a retriable
                    // error is resumed by the worker against this same
                    // upload_id, and the worker aborts + emits the terminal
                    // exactly once on final give-up / cancel. Aborting here
                    // would destroy the upload the next attempt wants to resume.
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

    /// Either resume an upload from saved state, or create a fresh multipart
    /// upload and return its id alongside an empty `completed_parts` list.
    ///
    /// Resume state is discarded when the source file changed since the saved
    /// parts were uploaded: part boundaries are byte offsets into one exact
    /// version of the file, so applying them to a different version would
    /// assemble a corrupted object. The stale server-side upload is aborted
    /// best-effort before starting fresh.
    async fn init_or_resume_upload(
        &self,
        bucket: &str,
        key: &str,
        opts: &PutOptions,
        ctx: &TransferCtx,
    ) -> AppResult<(String, Vec<SavedPart>)> {
        if let Some(state) = ctx.resume.clone() {
            if !state.upload_id.is_empty() && !resume_state_is_stale(&state, ctx) {
                return Ok((state.upload_id, state.completed_parts));
            }
            if !state.upload_id.is_empty() {
                // Source mismatch (or unknown-but-suspect state): drop the old
                // upload server-side so incomplete-multipart storage doesn't
                // leak. Best-effort — the fresh create below must proceed.
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
        Ok((id, Vec::new()))
    }

    /// Finalize a multipart upload by sending the complete list of ETags in
    /// ascending part-number order.
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

