//! FolderCtx for tests: real Db + TransferManager, an injected store, fast backoff, event hook.
#![allow(dead_code)] // shared by several test crates, each uses a subset

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cosmog_lib::db::accounts::NewAccount;
use cosmog_lib::db::folder_upload::JobState;
use cosmog_lib::db::Db;
use cosmog_lib::error::AppResult;
use cosmog_lib::folder_upload::{self as fu, FolderCtx, FolderJob, FolderJobs, Timing};
use cosmog_lib::store::ObjectStore;
use cosmog_lib::transfer::{TransferEvent, TransferManager};

pub type Hook = Arc<dyn Fn(&TransferEvent) + Send + Sync>;

#[derive(Clone)]
pub struct Ctx {
    pub db: Db,
    pub db_path: PathBuf,
    pub transfers: TransferManager,
    pub jobs: FolderJobs,
    pub store: Arc<dyn ObjectStore>,
    pub events: Arc<Mutex<Vec<TransferEvent>>>,
    pub hook: Arc<Mutex<Option<Hook>>>,
}

impl FolderCtx for Ctx {
    fn db(&self) -> &Db {
        &self.db
    }
    fn db_path(&self) -> &Path {
        &self.db_path
    }
    fn transfers(&self) -> &TransferManager {
        &self.transfers
    }
    fn jobs(&self) -> &FolderJobs {
        &self.jobs
    }
    async fn store_for(&self, _account_id: &str) -> AppResult<Arc<dyn ObjectStore>> {
        Ok(self.store.clone())
    }
    fn on_transfer_event(&self, _job_id: &str, ev: &TransferEvent) {
        self.events.lock().unwrap().push(ev.clone());
        let hook = self.hook.lock().unwrap().clone();
        if let Some(h) = hook {
            h(ev);
        }
    }
    fn timing(&self) -> Timing {
        Timing {
            net_backoff_min: Duration::from_millis(100),
            net_backoff_max: Duration::from_millis(400),
            retry_base: 1,
            retry_cap: 1,
            io_backoff: 1,
        }
    }
}

impl Ctx {
    /// Same DB, fresh engine (new runtime registry and transfer manager), as after a restart.
    pub fn restarted(&self, store: Arc<dyn ObjectStore>, concurrency: usize) -> Ctx {
        Ctx {
            db: self.db.clone(),
            db_path: self.db_path.clone(),
            transfers: TransferManager::new(self.db.clone(), concurrency),
            jobs: FolderJobs::new(),
            store,
            events: Arc::new(Mutex::new(Vec::new())),
            hook: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_hook(&self, h: impl Fn(&TransferEvent) + Send + Sync + 'static) {
        *self.hook.lock().unwrap() = Some(Arc::new(h));
    }
}

pub async fn make_ctx(store: Arc<dyn ObjectStore>, concurrency: usize) -> (Ctx, tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.sqlite");
    let db = Db::open(&db_path).await.unwrap();
    let acct = db
        .insert_account(NewAccount {
            name: "fu".into(),
            protocol: "s3".into(),
            endpoint: Some(crate::common::minio_endpoint()),
            region: crate::common::MINIO_REGION.into(),
            access_key_id: crate::common::MINIO_ACCESS_KEY.into(),
            addressing_style: Some("path".into()),
        })
        .await
        .unwrap();
    let mut s = db.settings_load().await.unwrap();
    s.transfer_concurrency = concurrency as u32;
    db.settings_save(s).await.unwrap();
    let ctx = Ctx {
        transfers: TransferManager::new(db.clone(), concurrency),
        db,
        db_path,
        jobs: FolderJobs::new(),
        store,
        events: Arc::new(Mutex::new(Vec::new())),
        hook: Arc::new(Mutex::new(None)),
    };
    (ctx, dir, acct.id)
}

/// Analyze, check the destination, return the job id.
pub async fn analyze_checked(ctx: &Ctx, acct: &str, bucket: &str, root: &Path, dest: &str) -> String {
    let a = fu::analyze(ctx, acct, bucket, root.to_str().unwrap(), dest, |_| {}).await.expect("analyze");
    fu::check_remote(ctx, &a.job_id, dest, |_| {}).await.expect("check remote");
    a.job_id
}

/// Polls until the job leaves queued/running.
pub async fn wait_settled(ctx: &Ctx, job_id: &str, secs: u64) -> FolderJob {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let j = fu::get_job(ctx, job_id).await.unwrap();
        if !matches!(j.state, JobState::Queued | JobState::Running) {
            return j;
        }
        assert!(std::time::Instant::now() < deadline, "job {job_id} still {:?}", j.state);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub async fn wait_until<F, Fut>(secs: u64, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    while !f().await {
        assert!(std::time::Instant::now() < deadline, "condition not met in {secs}s");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub fn write(root: &Path, rel: &str, body: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

/// 5 MiB parts and threshold (the S3 floor), one part at a time.
pub async fn small_parts(ctx: &Ctx) {
    let mut s = ctx.db.settings_load().await.unwrap();
    s.part_size_bytes = 5 * 1024 * 1024;
    s.multipart_threshold_bytes = 5 * 1024 * 1024;
    s.multipart_parallelism = 1;
    ctx.db.settings_save(s).await.unwrap();
}

/// The job's item at `rel`.
pub async fn item_at(ctx: &Ctx, job_id: &str, rel: &str) -> fu::FolderItem {
    let page = fu::list_items(ctx, job_id, cosmog_lib::db::folder_upload::ItemFilter::All, 0, 500).await.unwrap();
    page.items.into_iter().find(|i| i.rel_path == rel).unwrap_or_else(|| panic!("no item {rel}"))
}

pub fn body(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
}
