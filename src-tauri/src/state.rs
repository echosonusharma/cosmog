//! Process-wide shared state managed by Tauri.
//!
//! [`AppState`] is registered via `app.manage(...)` at startup and accessed from
//! command handlers through `State<'_, AppState>`. It is cheap to clone — every
//! field wraps its contents in `Arc` or has interior `Arc` sharing.
//!
//! The client cache memoizes one [`ObjectStore`] per account so we don't
//! reconstruct an AWS SDK client (and re-read the keyring secret) for every
//! invocation. Cache entries are invalidated on account update or delete via
//! [`AppState::invalidate`].
//!
//! `scan_cancels` indexes currently-running full bucket scans by
//! `(account_id, bucket)` so [`commands::search::cancel_bucket_scan`] can stop
//! one in flight.

use std::path::PathBuf;
use std::sync::Arc;

use dashmap::{DashMap, DashSet};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::db::settings::AppSettings;
use crate::db::Db;
use crate::error::AppResult;
use crate::providers::build_store;
use crate::store::ObjectStore;
use crate::transfer::TransferManager;

/// Shared backend state managed by Tauri.
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub transfers: TransferManager,
    /// Filesystem directory where the rolling log file lives. Used by the
    /// `get_log_tail` command.
    pub log_dir: PathBuf,
    /// Path to the live SQLite file. Used by backup/restore commands.
    pub db_path: PathBuf,
    /// account_id -> initialized [`ObjectStore`] client. Lazily populated.
    clients: Arc<DashMap<String, Arc<dyn ObjectStore>>>,
    /// (account_id, bucket) -> cancellation token for an active full bucket
    /// scan.
    scan_cancels: Arc<DashMap<(String, String), CancellationToken>>,
    /// Per-bulk-op cancellation tokens, keyed by a caller-chosen opaque id.
    /// Kept separate from `scan_cancels` so a `delete_folder` cancel can't
    /// kill a real bucket-scan and vice versa.
    bulk_cancels: Arc<DashMap<String, CancellationToken>>,
    /// In-flight prefix syncs: (account_id, bucket, prefix). Guards against
    /// concurrent syncs for the same prefix — FE polling can otherwise spawn
    /// multiple overlapping mark/sweep cycles that corrupt the cache.
    prefix_syncs: Arc<DashSet<(String, String, String)>>,
    /// In-memory cache for AppSettings. Avoids a SQLite read on every
    /// browse_prefix call (which is polled every 1.5s during refresh).
    /// Invalidated by settings_patch and restore_backup commands.
    settings_cache: Arc<RwLock<Option<AppSettings>>>,
}

impl AppState {
    pub fn new(db: Db, concurrency: usize, log_dir: PathBuf, db_path: PathBuf) -> Self {
        let transfers = TransferManager::new(db.clone(), concurrency);
        Self {
            db,
            transfers,
            log_dir,
            db_path,
            clients: Arc::new(DashMap::new()),
            scan_cancels: Arc::new(DashMap::new()),
            bulk_cancels: Arc::new(DashMap::new()),
            prefix_syncs: Arc::new(DashSet::new()),
            settings_cache: Arc::new(RwLock::new(None)),
        }
    }

    /// Load settings, using the in-memory cache when warm.
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

    /// Invalidate the settings cache. Call after any write to settings.
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
        // Fast path: already cached.
        if let Some(existing) = self.clients.get(account_id) {
            return Ok(existing.clone());
        }
        // Slow path: build client, then insert only if another caller hasn't
        // beaten us (or_insert is a no-op when the entry already exists).
        let account = self.db.get_account(account_id).await?;
        let store = build_store(&account).await?;
        Ok(self.clients
            .entry(account_id.to_string())
            .or_insert(store)
            .clone())
    }

    pub fn invalidate(&self, account_id: &str) {
        self.clients.remove(account_id);
    }

    /// Register an in-flight scan's cancel token. Returns a fresh token if the
    /// caller does not supply one, so the worker can listen on it.
    pub fn register_scan(&self, account_id: &str, bucket: &str) -> CancellationToken {
        let token = CancellationToken::new();
        self.scan_cancels.insert(
            (account_id.to_string(), bucket.to_string()),
            token.clone(),
        );
        token
    }

    /// Idempotent. Returns `Ok(())` even if no scan is registered (i.e. already
    /// terminal).
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

    /// True when a scan is currently registered for this (account, bucket).
    pub fn scan_in_flight(&self, account_id: &str, bucket: &str) -> bool {
        self.scan_cancels
            .contains_key(&(account_id.to_string(), bucket.to_string()))
    }

    /// Adjust the maximum number of concurrent transfers at runtime.
    pub fn set_transfer_concurrency(&self, n: usize) {
        self.transfers.set_concurrency(n);
    }

    /// Atomically claim a prefix sync slot. Returns `true` if this caller won
    /// the slot (should spawn the task), `false` if already in flight.
    /// Using `DashSet::insert` as the atomic check-and-set avoids the TOCTOU
    /// race between a separate `contains` + `insert`.
    pub fn claim_prefix_sync(&self, account_id: &str, bucket: &str, prefix: &str) -> bool {
        self.prefix_syncs.insert((
            account_id.to_string(),
            bucket.to_string(),
            prefix.to_string(),
        ))
    }

    /// Returns true if a background prefix sync is currently in flight.
    pub fn prefix_sync_in_flight(&self, account_id: &str, bucket: &str, prefix: &str) -> bool {
        self.prefix_syncs
            .contains(&(account_id.to_string(), bucket.to_string(), prefix.to_string()))
    }

    /// Returns true if any prefix sync is in flight for this (account, bucket).
    pub fn prefix_sync_in_flight_for_bucket(&self, account_id: &str, bucket: &str) -> bool {
        self.prefix_syncs
            .iter()
            .any(|entry| entry.0 == account_id && entry.1 == bucket)
    }

    /// Clear the in-flight marker. Call after the sync task finishes (success or error).
    pub fn unregister_prefix_sync(&self, account_id: &str, bucket: &str, prefix: &str) {
        self.prefix_syncs.remove(&(
            account_id.to_string(),
            bucket.to_string(),
            prefix.to_string(),
        ));
    }

    /// Cancel every active bucket scan for `account_id`. Used during account
    /// deletion to stop dangling workers.
    pub fn cancel_all_scans_for_account(&self, account_id: &str) {
        for entry in self.scan_cancels.iter() {
            if entry.key().0 == account_id {
                entry.value().cancel();
            }
        }
    }
}
