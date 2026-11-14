//! In-memory ObjectStore for engine tests: single PUT and multipart (threshold, upload ids,
//! resume), metadata and ETags, If-None-Match (native or HEAD-emulated), injectable failures.
#![allow(dead_code)] // shared by several test crates, each uses a subset

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use cosmog_lib::error::{AppError, AppResult};
use cosmog_lib::store::*;
use cosmog_lib::transfer::source_guard::open_checked;
use cosmog_lib::transfer::{DownloadResult, TransferCtx, TransferEvent, UploadResult};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

#[derive(Default, Clone)]
pub struct Upload {
    pub key: String,
    pub parts: BTreeMap<i32, Vec<u8>>,
    pub meta: HashMap<String, String>,
}

#[derive(Default)]
pub struct MemStore {
    pub objects: Mutex<BTreeMap<String, Vec<u8>>>,
    /// key -> (user metadata, etag)
    pub meta: Mutex<HashMap<String, (HashMap<String, String>, String)>>,
    pub delay: Mutex<Duration>,
    /// Error code every PUT, part, HEAD and head_bucket fails with while set.
    pub fail: Mutex<Option<&'static str>>,
    /// Error code every part fails with while set (head_bucket stays fine).
    pub part_fail: Mutex<Option<&'static str>>,
    /// Error code abort fails with while set.
    pub abort_fail: Mutex<Option<&'static str>>,
    /// Commits of PUT/complete whose response is "lost" (network error after the write).
    pub lose_responses: AtomicU64,
    /// Provider rejects If-None-Match: the store falls back to HEAD (shared `ensure_absent`).
    pub inm_unsupported: Mutex<bool>,
    /// Listings claim more results but give no continuation token.
    pub truncate_listing: Mutex<bool>,
    pub uploads: Mutex<HashMap<String, Upload>>,
    pub puts: Mutex<u64>,
    /// Every part sent: (upload_id, part number).
    pub parts_sent: Mutex<Vec<(String, i32)>>,
    pub heads: AtomicU64,
    pub aborted: Mutex<Vec<String>>,
    next: AtomicU64,
}

fn err(code: &str) -> AppError {
    match code {
        "network_unreachable" => AppError::NetworkUnreachable("injected".into()),
        "request_timeout" => AppError::RequestTimeout("injected".into()),
        "access_denied" => AppError::AccessDenied("injected".into()),
        "rate_limited" => AppError::RateLimited("injected".into()),
        "tls_error" => AppError::TlsError("injected".into()),
        _ => AppError::Internal("injected".into()),
    }
}

fn unsupported<T>() -> AppResult<T> {
    Err(AppError::Unsupported("mem store".into()))
}

impl MemStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn check(&self) -> AppResult<()> {
        match *self.fail.lock().unwrap() {
            Some(c) => Err(err(c)),
            None => Ok(()),
        }
    }

    pub fn keys(&self) -> Vec<String> {
        self.objects.lock().unwrap().keys().cloned().collect()
    }

    /// A foreign writer's object (no tag).
    pub fn put_object_bytes_sync(&self, key: &str, data: &[u8]) {
        self.objects.lock().unwrap().insert(key.into(), data.to_vec());
        let etag = format!("\"f{}\"", self.next.fetch_add(1, Ordering::SeqCst));
        self.meta.lock().unwrap().insert(key.into(), (HashMap::new(), etag));
    }

    pub fn open_uploads(&self) -> Vec<String> {
        self.uploads.lock().unwrap().keys().cloned().collect()
    }

    pub fn tag_of(&self, key: &str) -> Option<String> {
        self.meta.lock().unwrap().get(key).and_then(|(m, _)| m.get("cosmog-fu").cloned())
    }

    pub fn etag_of(&self, key: &str) -> Option<String> {
        self.meta.lock().unwrap().get(key).map(|(_, e)| e.clone())
    }

    /// Writes `body` unless the condition fails; a lost response still commits.
    fn commit(&self, key: &str, body: Vec<u8>, meta: HashMap<String, String>, inm: bool) -> AppResult<String> {
        let etag = format!("\"e{}\"", self.next.fetch_add(1, Ordering::SeqCst));
        {
            let mut objs = self.objects.lock().unwrap();
            if inm && objs.contains_key(key) {
                return Err(AppError::Conflict("412 PreconditionFailed".into()));
            }
            objs.insert(key.to_string(), body);
            self.meta.lock().unwrap().insert(key.to_string(), (meta, etag.clone()));
        }
        *self.puts.lock().unwrap() += 1;
        if self.lose_responses.load(Ordering::SeqCst) > 0 {
            self.lose_responses.fetch_sub(1, Ordering::SeqCst);
            return Err(AppError::NetworkUnreachable("response lost".into()));
        }
        Ok(etag)
    }

    /// Conditional commit as a provider would: native, or HEAD then plain write when unsupported.
    async fn commit_cond(&self, key: &str, body: Vec<u8>, meta: HashMap<String, String>, inm: bool) -> AppResult<String> {
        if inm && *self.inm_unsupported.lock().unwrap() {
            ensure_absent(self, "b", key).await?;
            return self.commit(key, body, meta, false);
        }
        self.commit(key, body, meta, inm)
    }

    async fn read(source: &Path, opts: &PutOptions, range: Option<(u64, u64)>) -> AppResult<Vec<u8>> {
        let mut f = match &opts.source_guard {
            Some(g) => open_checked(source, g).await?,
            None => tokio::fs::File::open(source).await?,
        };
        let mut buf = Vec::new();
        match range {
            Some((off, len)) => {
                f.seek(std::io::SeekFrom::Start(off)).await?;
                f.take(len).read_to_end(&mut buf).await?;
            }
            None => {
                f.read_to_end(&mut buf).await?;
            }
        }
        Ok(buf)
    }

    async fn multipart(&self, key: &str, source: &Path, total: u64, opts: &PutOptions, ctx: &TransferCtx) -> AppResult<UploadResult> {
        let part_size = ctx.resume.as_ref().and_then(|r| r.part_size).unwrap_or(ctx.part_size).max(1);
        let resumed = ctx.resume.as_ref().map(|r| r.upload_id.clone()).filter(|u| self.uploads.lock().unwrap().contains_key(u));
        let upload_id = match resumed {
            Some(u) => u,
            None => {
                let u = format!("up{}", self.next.fetch_add(1, Ordering::SeqCst));
                let up = Upload { key: key.into(), parts: BTreeMap::new(), meta: opts.user_metadata.clone() };
                self.uploads.lock().unwrap().insert(u.clone(), up);
                u
            }
        };
        if let Some(h) = &ctx.on_upload_id {
            h.call(&upload_id, part_size).await;
        }
        ctx.progress.emit(TransferEvent::MultipartInitiated { transfer_id: ctx.transfer_id.clone(), upload_id: upload_id.clone(), part_size });
        let n = total.div_ceil(part_size) as i32;
        for p in 1..=n {
            if self.uploads.lock().unwrap().get(&upload_id).is_some_and(|u| u.parts.contains_key(&p)) {
                continue;
            }
            if ctx.cancel.is_cancelled() {
                return Err(AppError::Canceled("mem".into()));
            }
            let delay = *self.delay.lock().unwrap();
            tokio::select! {
                _ = ctx.cancel.cancelled() => return Err(AppError::Canceled("mem".into())),
                _ = tokio::time::sleep(delay) => {}
            }
            self.check()?;
            if let Some(c) = *self.part_fail.lock().unwrap() {
                return Err(err(c));
            }
            let off = (p as u64 - 1) * part_size;
            let body = Self::read(source, opts, Some((off, part_size.min(total - off)))).await?;
            match self.uploads.lock().unwrap().get_mut(&upload_id) {
                Some(u) => u.parts.insert(p, body),
                None => return Err(AppError::UploadNotFound(upload_id.clone())),
            };
            self.parts_sent.lock().unwrap().push((upload_id.clone(), p));
            ctx.progress.emit(TransferEvent::PartCompleted { transfer_id: ctx.transfer_id.clone(), upload_id: upload_id.clone(), part_number: p, etag: format!("p{p}") });
        }
        let up = self.uploads.lock().unwrap().get(&upload_id).cloned().ok_or_else(|| AppError::UploadNotFound(upload_id.clone()))?;
        let body: Vec<u8> = up.parts.values().flatten().copied().collect();
        let etag = self.commit_cond(key, body, up.meta.clone(), opts.if_none_match.is_some()).await;
        if !matches!(etag, Err(AppError::Conflict(_))) {
            // Completed (even with a lost response): the upload no longer exists.
            self.uploads.lock().unwrap().remove(&upload_id);
        }
        Ok(UploadResult { etag: Some(etag?), upload_id: Some(upload_id) })
    }
}

#[async_trait]
impl ObjectStore for MemStore {
    async fn list_buckets(&self) -> AppResult<Vec<Bucket>> { unsupported() }
    async fn create_bucket(&self, _: &str, _: Option<&str>) -> AppResult<()> { unsupported() }
    async fn delete_bucket(&self, _: &str) -> AppResult<()> { unsupported() }
    async fn head_bucket(&self, _: &str) -> AppResult<()> { self.check() }
    async fn get_bucket_location(&self, _: &str) -> AppResult<Option<String>> { Ok(None) }
    async fn put_bucket_acl(&self, _: &str, _: CannedAcl) -> AppResult<()> { unsupported() }
    async fn get_bucket_versioning(&self, _: &str) -> AppResult<bool> { Ok(false) }
    async fn put_bucket_versioning(&self, _: &str, _: bool) -> AppResult<()> { unsupported() }
    async fn get_bucket_policy(&self, _: &str) -> AppResult<Option<String>> { Ok(None) }
    async fn put_bucket_policy(&self, _: &str, _: String) -> AppResult<()> { unsupported() }
    async fn delete_bucket_policy(&self, _: &str) -> AppResult<()> { unsupported() }
    async fn get_bucket_cors(&self, _: &str) -> AppResult<Option<CorsConfig>> { Ok(None) }
    async fn put_bucket_cors(&self, _: &str, _: CorsConfig) -> AppResult<()> { unsupported() }
    async fn delete_bucket_cors(&self, _: &str) -> AppResult<()> { unsupported() }

    async fn list_objects(&self, _: &str, opts: ListOptions) -> AppResult<ListPage> {
        let prefix = opts.prefix.unwrap_or_default();
        let mut objects = Vec::new();
        let mut prefixes = std::collections::BTreeSet::new();
        for (k, v) in self.objects.lock().unwrap().iter().filter(|(k, _)| k.starts_with(&prefix)) {
            let rest = &k[prefix.len()..];
            match (&opts.delimiter, rest.find('/')) {
                (Some(_), Some(i)) => {
                    prefixes.insert(format!("{prefix}{}", &rest[..=i]));
                }
                _ => objects.push(meta(k, v.len(), None, None)),
            }
        }
        let truncated = *self.truncate_listing.lock().unwrap();
        Ok(ListPage { objects, prefixes: prefixes.into_iter().collect(), continuation: None, is_truncated: truncated })
    }

    async fn head_object(&self, _: &str, key: &str) -> AppResult<ObjectMeta> {
        self.heads.fetch_add(1, Ordering::SeqCst);
        self.check()?;
        let m = self.meta.lock().unwrap().get(key).cloned();
        match self.objects.lock().unwrap().get(key) {
            Some(v) => Ok(meta(key, v.len(), m.as_ref().map(|m| m.0.clone()), m.map(|m| m.1))),
            None => Err(AppError::ObjectNotFound(key.into())),
        }
    }

    async fn create_folder(&self, _: &str, prefix: &str) -> AppResult<()> {
        self.check()?;
        self.objects.lock().unwrap().insert(format!("{}/", prefix.trim_end_matches('/')), Vec::new());
        Ok(())
    }

    async fn delete_object(&self, _: &str, key: &str) -> AppResult<()> {
        self.objects.lock().unwrap().remove(key);
        self.meta.lock().unwrap().remove(key);
        Ok(())
    }
    async fn delete_objects(&self, _: &str, _: &[String]) -> AppResult<DeleteObjectsResult> { unsupported() }
    async fn delete_object_version(&self, _: &str, _: &str, _: &str) -> AppResult<()> { unsupported() }
    async fn restore_object_version(&self, _: &str, _: &str, _: &str) -> AppResult<()> { unsupported() }
    async fn list_object_versions(&self, _: &str, _: Option<&str>, _: Option<String>) -> AppResult<(Vec<ObjectVersion>, Option<String>)> { unsupported() }
    async fn copy_object(&self, _: &str, _: &str, _: &str, _: &str) -> AppResult<()> { unsupported() }
    async fn put_object_acl(&self, _: &str, _: &str, _: CannedAcl) -> AppResult<()> { unsupported() }
    async fn presign_get(&self, _: &str, _: &str, _: u64) -> AppResult<String> { unsupported() }
    async fn read_object_range(&self, _: &str, _: &str, _: u64) -> AppResult<ObjectPreview> { unsupported() }
    async fn read_object_full(&self, _: &str, key: &str) -> AppResult<Vec<u8>> {
        self.objects.lock().unwrap().get(key).cloned().ok_or_else(|| AppError::ObjectNotFound(key.into()))
    }
    async fn get_object_tagging(&self, _: &str, _: &str) -> AppResult<Vec<ObjectTag>> { unsupported() }
    async fn put_object_tagging(&self, _: &str, _: &str, _: &[ObjectTag]) -> AppResult<()> { unsupported() }
    async fn delete_object_tagging(&self, _: &str, _: &str) -> AppResult<()> { unsupported() }

    async fn put_object(&self, _: &str, key: &str, source: PathBuf, opts: PutOptions, ctx: TransferCtx) -> AppResult<UploadResult> {
        let total = tokio::fs::metadata(&source).await?.len();
        ctx.progress.emit(TransferEvent::Started { transfer_id: ctx.transfer_id.clone(), bytes_total: Some(total) });
        if total > 0 && (total > ctx.multipart_threshold || ctx.resume.is_some()) {
            let r = self.multipart(key, &source, total, &opts, &ctx).await?;
            ctx.progress.emit(TransferEvent::Progress { transfer_id: ctx.transfer_id.clone(), bytes_done: total, bytes_total: Some(total) });
            return Ok(r);
        }
        let delay = *self.delay.lock().unwrap();
        tokio::select! {
            _ = ctx.cancel.cancelled() => return Err(AppError::Canceled("mem".into())),
            _ = tokio::time::sleep(delay) => {}
        }
        self.check()?;
        let body = Self::read(&source, &opts, None).await?;
        let etag = self.commit_cond(key, body, opts.user_metadata.clone(), opts.if_none_match.is_some()).await?;
        ctx.progress.emit(TransferEvent::Progress { transfer_id: ctx.transfer_id.clone(), bytes_done: total, bytes_total: Some(total) });
        Ok(UploadResult { etag: Some(etag), upload_id: None })
    }

    async fn put_object_bytes(&self, _: &str, key: &str, _: &str, data: Vec<u8>, m: HashMap<String, String>) -> AppResult<()> {
        self.commit(key, data, m, false).map(|_| ())
    }
    async fn get_object(&self, _: &str, _: &str, _: PathBuf, _: GetOptions, _: TransferCtx) -> AppResult<DownloadResult> { unsupported() }
    async fn abort_multipart_upload(&self, _: &str, _: &str, upload_id: &str) -> AppResult<()> {
        if let Some(c) = *self.abort_fail.lock().unwrap() {
            return Err(err(c));
        }
        self.aborted.lock().unwrap().push(upload_id.to_string());
        match self.uploads.lock().unwrap().remove(upload_id) {
            Some(_) => Ok(()),
            None => Err(AppError::UploadNotFound(upload_id.into())),
        }
    }
    async fn list_multipart_uploads(&self, _: &str, _: Option<&str>, _: Option<String>) -> AppResult<(Vec<PendingMultipartUpload>, Option<String>)> {
        Ok((Vec::new(), None))
    }
}

fn meta(key: &str, size: usize, user: Option<HashMap<String, String>>, etag: Option<String>) -> ObjectMeta {
    ObjectMeta {
        key: key.into(),
        size: size as i64,
        etag,
        last_modified: Some(0),
        storage_class: None,
        content_type: None,
        version_id: None,
        user_metadata: user.unwrap_or_default(),
    }
}
