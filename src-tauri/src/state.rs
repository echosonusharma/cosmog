//! Tauri-managed shared state, registered via `app.manage(...)`. Cheap to
//! clone — every field wraps its contents in `Arc` / interior `Arc` sharing.

use std::path::PathBuf;
use std::sync::Arc;

use dashmap::{DashMap, DashSet};
use tokio::sync::{Mutex as AsyncMutex, RwLock};
use tokio_util::sync::CancellationToken;

use crate::db::accounts::UpdateAccount;
use crate::db::settings::AppSettings;
use crate::db::Db;
use crate::error::AppResult;
use crate::providers::{build_probe_store, build_store};
use crate::store::logging::LoggingStore;
use crate::store::region_retry::RegionRetryStore;
use crate::store::ObjectStore;
use crate::transfer::TransferManager;

/// Shared backend state managed by Tauri; the `clients` map memoizes one
/// store per account so SDK clients (and keyring reads) aren't rebuilt per call.
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub transfers: TransferManager,
    pub app: tauri::AppHandle,
    /// Dir of the rolling log file; used by `get_log_tail`.
    pub log_dir: PathBuf,
    /// Live SQLite file path; used by backup/restore commands.
    pub db_path: PathBuf,
    /// account_id -> initialized store client, lazily populated.
    clients: Arc<DashMap<String, Arc<dyn ObjectStore>>>,
    /// (account_id, bucket) -> token for an active full bucket scan.
    scan_cancels: Arc<DashMap<(String, String), CancellationToken>>,
    /// Bulk-op cancel tokens keyed by caller-chosen opaque id; kept separate
    /// from `scan_cancels` so one cancel can't kill the other op type.
    bulk_cancels: Arc<DashMap<String, CancellationToken>>,
    /// In-flight prefix syncs; guards against overlapping mark/sweep cycles
    /// (FE polling would otherwise corrupt the cache).
    prefix_syncs: Arc<DashSet<(String, String, String)>>,
    /// Last sync error per prefix, throttling background respawn after failures.
    prefix_sync_errors: Arc<DashMap<(String, String, String), (i64, String)>>,
    /// In-memory AppSettings cache, avoiding a read on every polled browse
    /// call. Invalidated by settings_patch / restore_backup.
    settings_cache: Arc<RwLock<Option<AppSettings>>>,
    /// Per-bucket mutex serializing encryption enable/rotate/disable so two
    /// concurrent enables can't overwrite each other's keychain identities.
    encryption_locks: Arc<DashMap<(String, String), Arc<AsyncMutex<()>>>>,
    /// Night Watcher per-file in-flight claims; stops notify + periodic scan
    /// double-enqueueing the same file (atomic DashSet::insert claim).
    nw_inflight: Arc<DashSet<(String, String)>>,
    nw_scan_inflight: Arc<DashSet<String>>,
}

impl AppState {
    pub fn new(db: Db, concurrency: usize, log_dir: PathBuf, db_path: PathBuf, app: tauri::AppHandle) -> Self {
        let transfers = TransferManager::new(db.clone(), concurrency);
        Self {
            db,
            transfers,
            app,
            log_dir,
            db_path,
            clients: Arc::new(DashMap::new()),
            scan_cancels: Arc::new(DashMap::new()),
            bulk_cancels: Arc::new(DashMap::new()),
            prefix_syncs: Arc::new(DashSet::new()),
            prefix_sync_errors: Arc::new(DashMap::new()),
            settings_cache: Arc::new(RwLock::new(None)),
            encryption_locks: Arc::new(DashMap::new()),
            nw_inflight: Arc::new(DashSet::new()),
            nw_scan_inflight: Arc::new(DashSet::new()),
        }
    }

    pub fn nw_claim(&self, watch_id: &str, rel_path: &str) -> bool {
        self.nw_inflight
            .insert((watch_id.to_string(), rel_path.to_string()))
    }

    pub fn nw_unclaim(&self, watch_id: &str, rel_path: &str) {
        self.nw_inflight
            .remove(&(watch_id.to_string(), rel_path.to_string()));
    }

    pub fn nw_scan_claim(&self, watch_id: &str) -> bool {
        self.nw_scan_inflight.insert(watch_id.to_string())
    }

    pub fn nw_scan_unclaim(&self, watch_id: &str) {
        self.nw_scan_inflight.remove(watch_id);
    }

    pub fn encryption_lock(&self, account_id: &str, bucket: &str) -> Arc<AsyncMutex<()>> {
        self.encryption_locks
            .entry((account_id.to_string(), bucket.to_string()))
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    pub async fn load_settings(&self) -> AppResult<AppSettings> {
        {
            let r = self.settings_cache.read().await;
            if let Some(s) = r.as_ref() {
                return Ok(s.clone());
            }
        }
        let s = self.db.settings_load().await?;
        *self.settings_cache.write().await = Some(s.clone());
        Ok(s)
    }

    pub async fn invalidate_settings(&self) {
        *self.settings_cache.write().await = None;
    }

    pub fn register_bulk(&self, op_id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        self.bulk_cancels.insert(op_id.to_string(), token.clone());
        token
    }

    pub fn unregister_bulk(&self, op_id: &str) {
        self.bulk_cancels.remove(op_id);
    }

    pub fn cancel_bulk(&self, op_id: &str) {
        if let Some(t) = self.bulk_cancels.get(op_id) {
            t.cancel();
        }
    }

    pub async fn store_for(&self, account_id: &str) -> AppResult<Arc<dyn ObjectStore>> {
        if let Some(existing) = self.clients.get(account_id) {
            return Ok(existing.clone());
        }
        // Build then entry().or_insert: another caller may have raced us in.
        let account = self.db.get_account(account_id).await?;
        let mut inner = build_store(&account).await?;
        // Real AWS only: per-bucket region routing so PermanentRedirect never
        // surfaces to the FE.
        if account.endpoint.is_none() {
            inner = Arc::new(RegionRetryStore::new(inner, account.clone()));
        }
        let store: Arc<dyn ObjectStore> = Arc::new(LoggingStore::new(
            inner,
            self.db.clone(),
            self.app.clone(),
            &account.id,
            &account.name,
            account.endpoint.clone(),
            account.region.clone(),
        ));
        Ok(self.clients
            .entry(account_id.to_string())
            .or_insert(store)
            .clone())
    }

    pub fn invalidate(&self, account_id: &str) {
        self.clients.remove(account_id);
    }

    /// On PermanentRedirect: probe the bucket's real region via a store pointed
    /// at the global endpoint, persist it, evict the client, rebuild, return.
    pub async fn fix_region_for_bucket(
        &self,
        account_id: &str,
        bucket: &str,
    ) -> AppResult<Arc<dyn ObjectStore>> {
        let account = self.db.get_account(account_id).await?;
        let probe = build_probe_store(&account).await?;
        // Never persist a guessed region: a failed probe (e.g. IAM denies
        // GetBucketLocation) must not clobber a possibly-correct stored region.
        let real_region = probe
            .get_bucket_location(bucket)
            .await
            .map_err(|e| {
                tracing::warn!(bucket = %bucket, "region probe failed, keeping stored region: {e}");
                e
            })?
            .unwrap_or_else(|| "us-east-1".to_string());
        tracing::info!(
            account_id = %account_id,
            bucket = %bucket,
            region = %real_region,
            "PermanentRedirect: auto-correcting stored region"
        );
        self.db
            .update_account(
                account_id,
                UpdateAccount {
                    name: None,
                    endpoint: None,
                    region: Some(real_region),
                    access_key_id: None,
                    addressing_style: None,
                },
            )
            .await?;
        self.invalidate(account_id);
        self.store_for(account_id).await
    }

    pub fn register_scan(&self, account_id: &str, bucket: &str) -> CancellationToken {
        let token = CancellationToken::new();
        self.scan_cancels.insert(
            (account_id.to_string(), bucket.to_string()),
            token.clone(),
        );
        token
    }

    /// Atomically claim the scan slot; `None` if one is already in flight.
    /// The entry API prevents two concurrent enables from running overlapping
    /// scans that would corrupt each other's `seen` markers.
    pub fn try_register_scan(&self, account_id: &str, bucket: &str) -> Option<CancellationToken> {
        use dashmap::mapref::entry::Entry;
        match self
            .scan_cancels
            .entry((account_id.to_string(), bucket.to_string()))
        {
            Entry::Occupied(_) => None,
            Entry::Vacant(v) => {
                let token = CancellationToken::new();
                v.insert(token.clone());
                Some(token)
            }
        }
    }

    /// Idempotent; a no-op when no scan is registered.
    pub fn cancel_scan(&self, account_id: &str, bucket: &str) {
        if let Some(token) = self
            .scan_cancels
            .get(&(account_id.to_string(), bucket.to_string()))
        {
            token.cancel();
        }
    }

    pub fn unregister_scan(&self, account_id: &str, bucket: &str) {
        self.scan_cancels
            .remove(&(account_id.to_string(), bucket.to_string()));
    }

    pub fn scan_in_flight(&self, account_id: &str, bucket: &str) -> bool {
        self.scan_cancels
            .contains_key(&(account_id.to_string(), bucket.to_string()))
    }

    pub fn set_transfer_concurrency(&self, n: usize) {
        self.transfers.set_concurrency(n);
    }

    /// Atomically claim a sync slot; `DashSet::insert` avoids the
    /// contains+insert TOCTOU race.
    pub fn claim_prefix_sync(&self, account_id: &str, bucket: &str, prefix: &str) -> bool {
        self.prefix_syncs.insert((
            account_id.to_string(),
            bucket.to_string(),
            prefix.to_string(),
        ))
    }

    pub fn prefix_sync_in_flight(&self, account_id: &str, bucket: &str, prefix: &str) -> bool {
        self.prefix_syncs
            .contains(&(account_id.to_string(), bucket.to_string(), prefix.to_string()))
    }

    pub fn prefix_sync_in_flight_for_bucket(&self, account_id: &str, bucket: &str) -> bool {
        self.prefix_syncs
            .iter()
            .any(|entry| entry.0 == account_id && entry.1 == bucket)
    }

    pub fn unregister_prefix_sync(&self, account_id: &str, bucket: &str, prefix: &str) {
        self.prefix_syncs.remove(&(
            account_id.to_string(),
            bucket.to_string(),
            prefix.to_string(),
        ));
    }

    pub fn record_prefix_sync_error(&self, account_id: &str, bucket: &str, prefix: &str, err: &str) {
        let ts = chrono::Utc::now().timestamp();
        self.prefix_sync_errors.insert(
            (account_id.to_string(), bucket.to_string(), prefix.to_string()),
            (ts, err.to_string()),
        );
    }

    pub fn clear_prefix_sync_error(&self, account_id: &str, bucket: &str, prefix: &str) {
        self.prefix_sync_errors.remove(&(
            account_id.to_string(),
            bucket.to_string(),
            prefix.to_string(),
        ));
    }

    pub fn recent_prefix_sync_error(
        &self,
        account_id: &str,
        bucket: &str,
        prefix: &str,
        cooldown_secs: i64,
    ) -> Option<String> {
        let key = (account_id.to_string(), bucket.to_string(), prefix.to_string());
        let entry = self.prefix_sync_errors.get(&key)?;
        let now = chrono::Utc::now().timestamp();
        if now - entry.0 < cooldown_secs {
            Some(entry.1.clone())
        } else {
            None
        }
    }

    /// Cancel every active scan for an account (used during account deletion).
    pub fn cancel_all_scans_for_account(&self, account_id: &str) {
        for entry in self.scan_cancels.iter() {
            if entry.key().0 == account_id {
                entry.value().cancel();
            }
        }
    }
}
