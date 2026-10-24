//! [`ObjectStore`] over the AWS S3 SDK, for any S3-compatible endpoint.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::config::{Region, SharedCredentialsProvider};
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::types::{BucketVersioningStatus, VersioningConfiguration};
use aws_sdk_s3::Client;
use aws_smithy_types::error::metadata::ProvideErrorMetadata;
use aws_smithy_types::byte_stream::ByteStream;
use crate::error::{AppError, AppResult};
use classify::classify_aws;
pub use classify::http_status_in;
use crate::transfer::{DownloadResult, TransferCtx, TransferEvent, UploadResult};

use super::{
    Bucket, CannedAcl, CorsConfig, CorsRule, DeleteObjectError, DeleteObjectsResult, GetOptions,
    ListOptions, ListPage, ObjectMeta, ObjectPreview, ObjectStore, ObjectTag, ObjectVersion,
    PendingMultipartUpload, PutOptions,
};

mod classify;
mod copy;
mod download;
mod upload;
mod util;

use util::s3_err;

/// `endpoint` None = plain AWS. `addressing_style`: "path" | "virtual" | "auto" (path-style when an endpoint is set).
#[derive(Debug, Clone)]
pub struct S3Config {
    pub region: String,
    pub endpoint: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub addressing_style: String,
}

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

#[async_trait]
impl ObjectStore for S3Store {
    async fn list_buckets(&self) -> AppResult<Vec<Bucket>> {
        // Paginate: large accounts otherwise see only the first 1000-bucket page.
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
            // us-east-1 is implicit in S3; sending its LocationConstraint is illegal.
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
            .map_err(|e| match classify_aws("delete_bucket", e) {
                // Some providers answer a non-empty delete with a bare 409; named conflicts stay as-is.
                AppError::Conflict(d) if !d.contains("OperationAborted") && !d.contains("ConditionalRequestConflict") => {
                    AppError::BucketNotEmpty(d)
                }
                other => other,
            })?;
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
        // Empty LocationConstraint = us-east-1 (None for callers); legacy "EU" = eu-west-1.
        Ok(resp
            .location_constraint()
            .map(|c| c.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| if s == "EU" { "eu-west-1".to_string() } else { s.to_string() }))
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
                // NoSuchBucketPolicy = absent policy, not an error.
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
        // SDK already strips the `x-amz-meta-` prefix from metadata keys.
        let user_metadata = resp
            .metadata()
            .map(|m| m.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
            .unwrap_or_default();
        Ok(ObjectMeta {
            key: key.to_string(),
            size: resp.content_length().unwrap_or_default(),
            etag: resp.e_tag().map(|s| s.to_string()),
            last_modified: resp.last_modified().map(|d| d.secs()),
            // S3 omits the header for STANDARD objects.
            storage_class: Some(
                resp.storage_class().map_or("STANDARD", |s| s.as_str()).to_string(),
            ),
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
        // Restore = copy the old version onto the same key (invalid against delete markers).
        self.server_copy("restore_object_version", bucket, key, Some(version_id), bucket, key)
            .await
    }

    async fn list_object_versions(
        &self,
        bucket: &str,
        prefix: Option<&str>,
        continuation: Option<String>,
    ) -> AppResult<(Vec<ObjectVersion>, Option<String>)> {
        // Paging within one key needs BOTH key-marker and version-id-marker or lists truncate; carried as a JSON token.
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
        self.server_copy("copy_object", src_bucket, src_key, None, dst_bucket, dst_key)
            .await
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
        self.read_range(bucket, key, max_bytes).await
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
        // Paging needs key-marker AND upload-id-marker (many uploads per key), as a JSON token.
        #[derive(serde::Serialize, serde::Deserialize)]
        struct UploadsMarker {
            key: String,
            upload_id: Option<String>,
        }

        let mut req = self.client.list_multipart_uploads().bucket(bucket);
        if let Some(p) = prefix {
            req = req.prefix(p);
        }
        if let Some(token) = key_marker {
            match serde_json::from_str::<UploadsMarker>(&token) {
                Ok(m) => {
                    req = req.key_marker(m.key);
                    if let Some(uid) = m.upload_id {
                        req = req.upload_id_marker(uid);
                    }
                }
                // Legacy plain key-marker token.
                Err(_) => req = req.key_marker(token),
            }
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
            resp.next_key_marker().map(|k| {
                let m = UploadsMarker {
                    key: k.to_string(),
                    upload_id: resp
                        .next_upload_id_marker()
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string()),
                };
                serde_json::to_string(&m).unwrap_or_default()
            })
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
        self.download(bucket, key, dest, opts, ctx).await
    }
}
