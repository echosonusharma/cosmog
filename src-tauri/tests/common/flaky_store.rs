//! Wraps a real store and simulates losing the network: after N parts of an upload it goes
//! offline (uploads, HEADs and head_bucket fail with network_unreachable) until switched back.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cosmog_lib::error::{AppError, AppResult};
use cosmog_lib::store::*;
use cosmog_lib::transfer::{DownloadResult, ProgressSink, TransferCtx, TransferEvent, UploadResult};
use tokio_util::sync::CancellationToken;

pub struct FlakyStore {
    pub inner: Arc<dyn ObjectStore>,
    pub offline: AtomicBool,
    /// Go offline once this many parts (in total) have reached the provider.
    pub cut_after_parts: Mutex<Option<usize>>,
    parts: Arc<AtomicUsize>,
}

fn net() -> AppError {
    AppError::NetworkUnreachable("simulated network loss".into())
}

fn unsupported<T>() -> AppResult<T> {
    Err(AppError::Unsupported("flaky store".into()))
}

impl FlakyStore {
    pub fn new(inner: Arc<dyn ObjectStore>) -> Arc<Self> {
        Arc::new(Self { inner, offline: AtomicBool::new(false), cut_after_parts: Mutex::new(None), parts: Default::default() })
    }

    fn online(&self) -> AppResult<()> {
        if self.offline.load(Ordering::SeqCst) {
            return Err(net());
        }
        Ok(())
    }
}

#[async_trait]
impl ObjectStore for FlakyStore {
    async fn list_buckets(&self) -> AppResult<Vec<Bucket>> { unsupported() }
    async fn create_bucket(&self, _: &str, _: Option<&str>) -> AppResult<()> { unsupported() }
    async fn delete_bucket(&self, _: &str) -> AppResult<()> { unsupported() }
    async fn head_bucket(&self, b: &str) -> AppResult<()> {
        self.online()?;
        self.inner.head_bucket(b).await
    }
    async fn get_bucket_location(&self, b: &str) -> AppResult<Option<String>> { self.inner.get_bucket_location(b).await }
    async fn put_bucket_acl(&self, _: &str, _: CannedAcl) -> AppResult<()> { unsupported() }
    async fn get_bucket_versioning(&self, b: &str) -> AppResult<bool> { self.inner.get_bucket_versioning(b).await }
    async fn put_bucket_versioning(&self, _: &str, _: bool) -> AppResult<()> { unsupported() }
    async fn get_bucket_policy(&self, _: &str) -> AppResult<Option<String>> { Ok(None) }
    async fn put_bucket_policy(&self, _: &str, _: String) -> AppResult<()> { unsupported() }
    async fn delete_bucket_policy(&self, _: &str) -> AppResult<()> { unsupported() }
    async fn get_bucket_cors(&self, _: &str) -> AppResult<Option<CorsConfig>> { Ok(None) }
    async fn put_bucket_cors(&self, _: &str, _: CorsConfig) -> AppResult<()> { unsupported() }
    async fn delete_bucket_cors(&self, _: &str) -> AppResult<()> { unsupported() }
    async fn list_objects(&self, b: &str, o: ListOptions) -> AppResult<ListPage> { self.inner.list_objects(b, o).await }
    async fn head_object(&self, b: &str, k: &str) -> AppResult<ObjectMeta> {
        self.online()?;
        self.inner.head_object(b, k).await
    }
    async fn create_folder(&self, b: &str, p: &str) -> AppResult<()> {
        self.online()?;
        self.inner.create_folder(b, p).await
    }
    async fn create_folder_if_absent(&self, b: &str, p: &str) -> AppResult<bool> {
        self.online()?;
        self.inner.create_folder_if_absent(b, p).await
    }
    async fn delete_object(&self, b: &str, k: &str) -> AppResult<()> { self.inner.delete_object(b, k).await }
    async fn delete_objects(&self, b: &str, k: &[String]) -> AppResult<DeleteObjectsResult> { self.inner.delete_objects(b, k).await }
    async fn delete_object_version(&self, _: &str, _: &str, _: &str) -> AppResult<()> { unsupported() }
    async fn restore_object_version(&self, _: &str, _: &str, _: &str) -> AppResult<()> { unsupported() }
    async fn list_object_versions(&self, _: &str, _: Option<&str>, _: Option<String>) -> AppResult<(Vec<ObjectVersion>, Option<String>)> { unsupported() }
    async fn copy_object(&self, _: &str, _: &str, _: &str, _: &str) -> AppResult<()> { unsupported() }
    async fn put_object_acl(&self, _: &str, _: &str, _: CannedAcl) -> AppResult<()> { unsupported() }
    async fn presign_get(&self, _: &str, _: &str, _: u64) -> AppResult<String> { unsupported() }
    async fn read_object_range(&self, _: &str, _: &str, _: u64) -> AppResult<ObjectPreview> { unsupported() }
    async fn read_object_full(&self, b: &str, k: &str) -> AppResult<Vec<u8>> { self.inner.read_object_full(b, k).await }
    async fn get_object_tagging(&self, _: &str, _: &str) -> AppResult<Vec<ObjectTag>> { unsupported() }
    async fn put_object_tagging(&self, _: &str, _: &str, _: &[ObjectTag]) -> AppResult<()> { unsupported() }
    async fn delete_object_tagging(&self, _: &str, _: &str) -> AppResult<()> { unsupported() }

    async fn put_object(&self, b: &str, k: &str, src: PathBuf, opts: PutOptions, mut ctx: TransferCtx) -> AppResult<UploadResult> {
        self.online()?;
        // Cut the connection (cancel the inner request) once enough parts went through.
        let cut = CancellationToken::new();
        let outer = ctx.cancel.clone();
        let (c, o) = (cut.clone(), outer.clone());
        tokio::spawn(async move {
            o.cancelled().await;
            c.cancel();
        });
        let (parts, limit, sink, c2) = (self.parts.clone(), *self.cut_after_parts.lock().unwrap(), ctx.progress.clone(), cut.clone());
        let went_offline = Arc::new(AtomicBool::new(false));
        let w = went_offline.clone();
        ctx.progress = ProgressSink::from_fn(move |ev| {
            let part = matches!(ev, TransferEvent::PartCompleted { .. });
            sink.emit(ev);
            if part && limit.is_some_and(|n| parts.fetch_add(1, Ordering::SeqCst) + 1 >= n) {
                w.store(true, Ordering::SeqCst);
                c2.cancel();
            }
        });
        ctx.cancel = cut;
        let r = self.inner.put_object(b, k, src, opts, ctx).await;
        if went_offline.load(Ordering::SeqCst) && !outer.is_cancelled() {
            self.offline.store(true, Ordering::SeqCst);
            *self.cut_after_parts.lock().unwrap() = None;
            return Err(net());
        }
        r
    }

    async fn put_object_bytes(&self, b: &str, k: &str, ct: &str, d: Vec<u8>, m: HashMap<String, String>) -> AppResult<()> {
        self.inner.put_object_bytes(b, k, ct, d, m).await
    }
    async fn get_object(&self, _: &str, _: &str, _: PathBuf, _: GetOptions, _: TransferCtx) -> AppResult<DownloadResult> { unsupported() }
    async fn abort_multipart_upload(&self, b: &str, k: &str, u: &str) -> AppResult<()> {
        self.online()?;
        self.inner.abort_multipart_upload(b, k, u).await
    }
    async fn list_multipart_uploads(&self, b: &str, p: Option<&str>, m: Option<String>) -> AppResult<(Vec<PendingMultipartUpload>, Option<String>)> {
        self.inner.list_multipart_uploads(b, p, m).await
    }
}
