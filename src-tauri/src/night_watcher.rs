//! Night Watcher: one-way local-dir → S3-prefix sync. Periodic full scan is the source of truth; the
//! notify watcher is an accelerator only (Android has no inotify under SAF). Detection = mtime+size fast-path then blake3; delete_policy="keep" never deletes remote.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::db::night_watcher::{FileState, NightWatch};
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::store::{ObjectStore, PutOptions};
use crate::transfer::{ProgressSink, TransferEvent, TransferManager};

/// Slice of `AppState` behind this trait so integration tests can drive the *real* reconcile core
/// with an injected MinIO store + `Db` + `TransferManager`; `AppState` is the sole implementor.
#[allow(async_fn_in_trait)]
pub trait NwCtx: Clone + Send + Sync + 'static {
    fn db(&self) -> &Db;
    fn db_path(&self) -> &Path;
    fn transfers(&self) -> &TransferManager;
    fn store_for(
        &self,
        account_id: &str,
    ) -> impl std::future::Future<Output = AppResult<Arc<dyn ObjectStore>>> + Send;
    fn nw_claim(&self, watch_id: &str, rel_path: &str) -> bool;
    fn nw_unclaim(&self, watch_id: &str, rel_path: &str);
    fn nw_scan_claim(&self, watch_id: &str) -> bool;
    fn nw_scan_unclaim(&self, watch_id: &str);
}

impl NwCtx for AppState {
    fn db(&self) -> &Db {
        &self.db
    }
    fn db_path(&self) -> &Path {
        &self.db_path
    }
    fn transfers(&self) -> &TransferManager {
        &self.transfers
    }
    async fn store_for(&self, account_id: &str) -> AppResult<Arc<dyn ObjectStore>> {
        AppState::store_for(self, account_id).await
    }
    fn nw_claim(&self, watch_id: &str, rel_path: &str) -> bool {
        AppState::nw_claim(self, watch_id, rel_path)
    }
    fn nw_unclaim(&self, watch_id: &str, rel_path: &str) {
        AppState::nw_unclaim(self, watch_id, rel_path)
    }
    fn nw_scan_claim(&self, watch_id: &str) -> bool {
        AppState::nw_scan_claim(self, watch_id)
    }
    fn nw_scan_unclaim(&self, watch_id: &str) {
        AppState::nw_scan_unclaim(self, watch_id)
    }
}

/// Loop tick only; per-watch cadence comes from `full_scan_secs`.
const TICK_SECS: u64 = 30;

/// Max consecutive upload failures for one file before it is paused.
const MAX_UPLOAD_RETRIES: i64 = 3;
/// How long a file is skipped after hitting [`MAX_UPLOAD_RETRIES`] failures.
const RETRY_PAUSE_SECS: i64 = 3600;

/// Spawn the Night Watcher; returns immediately. Caller keeps the token alive for the process.
// Desktop only: Android's loop lives in the :nightwatch service process, so this in-process
// spawn is gated off to keep a second reconcile loop from racing the service's writes.
#[cfg(not(target_os = "android"))]
pub fn spawn(state: AppState) -> CancellationToken {
    let cancel = CancellationToken::new();
    {
        let token = cancel.clone();
        let st = state.clone();
        tokio::spawn(async move { desktop::run_watchers(st, token).await });
    }

    let token = cancel.clone();
    tokio::spawn(run_loop(state, token));

    cancel
}

/// Periodic full-scan loop shared by every host; generic over [`NwCtx`] so the Android headless
/// service process can drive the same reconcile core. Runs until the token cancels.
pub async fn run_loop<S: NwCtx>(ctx: S, token: CancellationToken) {
    info!("night watcher started");
    let mut tick = tokio::time::interval(Duration::from_secs(TICK_SECS));
    loop {
        tokio::select! {
            _ = token.cancelled() => {
                info!("night watcher stopped");
                return;
            }
            _ = tick.tick() => {
                if let Err(e) = run_once(&ctx, &token).await {
                    warn!("night watcher tick failed: {e}");
                }
            }
        }
    }
}

/// RAII guard releasing a watch's full-scan claim on drop, so a panic or early return
/// in the scan task can never strand the claim.
struct ScanClaimGuard<S: NwCtx> {
    state: S,
    watch_id: String,
}

impl<S: NwCtx> Drop for ScanClaimGuard<S> {
    fn drop(&mut self) {
        self.state.nw_scan_unclaim(&self.watch_id);
    }
}

/// Scan every due enabled watch, one guarded task per watch (never concurrent scans of one
/// watch). Token threads into scans so a stop also halts in-flight uploads (wakelock).
async fn run_once<S: NwCtx>(state: &S, token: &CancellationToken) -> AppResult<()> {
    let watches = state.db().list_enabled_watches().await?;
    let now = Utc::now().timestamp();
    for w in watches {
        if token.is_cancelled() {
            break;
        }
        let due = w
            .last_scan_at
            .map(|t| now - t >= w.full_scan_secs)
            .unwrap_or(true);
        if !due {
            continue;
        }
        if !state.nw_scan_claim(&w.id) {
            continue;
        }
        let state = state.clone();
        let task_token = token.clone();
        tokio::spawn(async move {
            let _guard = ScanClaimGuard {
                state: state.clone(),
                watch_id: w.id.clone(),
            };
            if task_token.is_cancelled() {
                return;
            }
            let res = reconcile_watch(&state, &w, &task_token).await;
            let err = res.as_ref().err().map(|e| e.to_string());
            if let Err(ref e) = res {
                warn!(watch = %w.id, "night watcher scan failed: {e}");
            }
            let _ = state
                .db()
                .set_watch_scan_result(&w.id, Utc::now().timestamp(), err)
                .await;
        });
    }
    Ok(())
}

/// Walk a watch's location and reconcile every non-ignored file; per-file errors are
/// logged, never fatal. Two modes: real fs dir (desktop) or SAF tree via `crate::saf`.
pub(crate) async fn reconcile_watch<S: NwCtx>(
    state: &S,
    watch: &NightWatch,
    token: &CancellationToken,
) -> AppResult<()> {
    if watch.tree_uri.is_some() {
        return reconcile_watch_saf(state, watch, token).await;
    }
    let root = PathBuf::from(&watch.local_dir);
    if !root.is_dir() {
        return Err(AppError::InvalidInput(format!(
            "watched dir does not exist: {}",
            root.display()
        )));
    }
    let matcher = Matcher::build(watch);
    // read_errors seeds `errors` so a subdir vanishing mid-scan blocks the sweep.
    let (files, read_errors) = collect_files(root.clone()).await?;
    // Bulk-load state once: the per-file fast-path is an in-memory lookup, not a DB round-trip.
    let prefetched = state.db().file_state_map(&watch.id).await?;
    let (mut scanned, mut ignored, mut enqueued, mut errors) = (0u64, 0u64, 0u64, read_errors);
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for rel in files {
        // A partial walk must never sweep: its incomplete seen set would mass-delete live rows.
        if token.is_cancelled() {
            info!(watch = %watch.id, "night watcher: scan aborted by cancellation");
            return Ok(());
        }
        scanned += 1;
        if matcher.is_ignored(&rel) {
            ignored += 1;
            debug!(watch = %watch.id, rel = %rel, "night watcher: ignored");
            continue;
        }
        let abs = root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        match reconcile_file(state, watch, &rel, &abs, Some(&prefetched)).await {
            Ok(r) => {
                if r.enqueued {
                    enqueued += 1;
                }
                if r.seen {
                    seen.insert(rel.clone());
                }
            }
            Err(e) => {
                errors += 1;
                warn!(watch = %watch.id, rel = %rel, "night watcher: reconcile failed: {e}");
            }
        }
    }
    // Mark-and-sweep, only on a clean walk: a transient read failure must not mass-prune.
    // delete_policy=keep drops just the state row; the remote object is untouched.
    let pruned = if errors == 0 {
        sweep_deleted(state, watch, &seen).await
    } else {
        0
    };
    info!(
        watch = %watch.id,
        dir = %watch.local_dir,
        scanned, ignored, enqueued, errors, pruned,
        "night watcher: scan complete"
    );
    Ok(())
}

/// Prune state rows for files absent from a completed full scan; returns rows removed.
/// delete_policy=keep: the remote object is left untouched.
async fn sweep_deleted<S: NwCtx>(
    state: &S,
    watch: &NightWatch,
    seen: &std::collections::HashSet<String>,
) -> u64 {
    let known = match state.db().file_state_list_rel_paths(&watch.id).await {
        Ok(k) => k,
        Err(e) => {
            warn!(watch = %watch.id, "night watcher: sweep list failed: {e}");
            return 0;
        }
    };
    let doomed: Vec<String> = known.into_iter().filter(|rel| !seen.contains(rel)).collect();
    if doomed.is_empty() {
        return 0;
    }
    for rel in &doomed {
        info!(watch = %watch.id, rel = %rel, "night watcher: local file gone, remote kept (delete_policy=keep)");
    }
    match state.db().file_state_delete_many(&watch.id, &doomed).await {
        Ok(n) => n,
        Err(e) => {
            warn!(watch = %watch.id, "night watcher: sweep delete failed: {e}");
            0
        }
    }
}

/// SAF-tree variant of [`reconcile_watch`] (Android): enumerate via `crate::saf`, fast-path
/// on the provider's mtime/size, hashing/staging only entries that actually changed.
async fn reconcile_watch_saf<S: NwCtx>(
    state: &S,
    watch: &NightWatch,
    token: &CancellationToken,
) -> AppResult<()> {
    let uri = watch.tree_uri.clone().expect("tree_uri present");
    // read_errors seeds `errors` (blocks sweep), matching the desktop path.
    let (entries, read_errors) = crate::saf::collect_tree_files(uri)
        .await
        .map_err(AppError::Internal)?;

    let matcher = Matcher::build(watch);
    let prefetched = state.db().file_state_map(&watch.id).await?;
    let (mut scanned, mut ignored, mut enqueued, mut errors) = (0u64, 0u64, 0u64, read_errors);
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for entry in entries {
        if entry.is_dir {
            continue;
        }
        if token.is_cancelled() {
            info!(watch = %watch.id, "night watcher: SAF scan aborted by cancellation");
            return Ok(());
        }
        scanned += 1;
        if matcher.is_ignored(&entry.rel_path) {
            ignored += 1;
            debug!(watch = %watch.id, rel = %entry.rel_path, "night watcher: ignored");
            continue;
        }
        match reconcile_saf_entry(state, watch, &entry, Some(&prefetched)).await {
            Ok(r) => {
                if r.enqueued {
                    enqueued += 1;
                }
                if r.seen {
                    seen.insert(entry.rel_path.clone());
                }
            }
            Err(e) => {
                errors += 1;
                warn!(watch = %watch.id, rel = %entry.rel_path, "night watcher: reconcile failed: {e}");
            }
        }
    }
    // Mark-and-sweep only after a clean enumeration+reconcile: transient failures must not mass-prune.
    let pruned = if errors == 0 {
        sweep_deleted(state, watch, &seen).await
    } else {
        0
    };
    info!(
        watch = %watch.id,
        tree = %watch.tree_uri.as_deref().unwrap_or(""),
        scanned, ignored, enqueued, errors, pruned,
        "night watcher: SAF scan complete"
    );
    Ok(())
}

/// Outcome of reconciling one entry. `seen=false` (only on a transient stat/metadata error)
/// keeps mark-and-sweep from pruning a file we merely failed to read once.
pub(crate) struct Reconciled {
    enqueued: bool,
    seen: bool,
}

/// Reconcile one filesystem file (desktop): stat, then the shared reconcile tail reading
/// straight from the fs path. A transient stat error leaves recorded state untouched.
pub(crate) async fn reconcile_file<S: NwCtx>(
    state: &S,
    watch: &NightWatch,
    rel_path: &str,
    abs: &Path,
    prefetched: Option<&std::collections::HashMap<String, FileState>>,
) -> AppResult<Reconciled> {
    let meta = match tokio::fs::metadata(abs).await {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Genuinely gone: leave unseen so mark-and-sweep prunes the row.
            return Ok(Reconciled { enqueued: false, seen: false });
        }
        Err(e) => {
            // Transient failure (AV/indexer lock, brief EIO): error so the sweep stays blocked;
            // seen:false would prune the row and force a redundant re-upload next scan.
            warn!(watch = %watch.id, rel = %rel_path, "night watcher: stat failed, blocking sweep this scan: {e}");
            return Err(AppError::Internal(format!(
                "stat {} failed: {e}",
                abs.display()
            )));
        }
    };
    if !meta.is_file() {
        return Ok(Reconciled { enqueued: false, seen: false });
    }
    let size = meta.len() as i64;
    // Millisecond precision: whole-second truncation made same-second edits invisible forever.
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let abs = abs.to_path_buf();
    let enqueued = reconcile_entry(
        state,
        watch,
        rel_path,
        mtime,
        size,
        prefetched,
        // hash: stream the fs file through blake3.
        {
            let abs = abs.clone();
            move || async move { hash_file(&abs).await }
        },
        // upload source: the fs path is already usable as-is.
        move || {
            let abs = abs.clone();
            async move { Ok(UploadSource { path: abs, cleanup_dir: None }) }
        },
    )
    .await?;
    Ok(Reconciled { enqueued, seen: true })
}

/// Reconcile one SAF tree entry (Android), mirroring [`reconcile_file`] with provider-supplied
/// mtime/size; hash/stage only when the shared tail decides an upload is required.
async fn reconcile_saf_entry<S: NwCtx>(
    state: &S,
    watch: &NightWatch,
    entry: &crate::saf::SafEntry,
    prefetched: Option<&std::collections::HashMap<String, FileState>>,
) -> AppResult<Reconciled> {
    let rel_path = entry.rel_path.clone();
    let doc_uri = entry.doc_uri.clone();
    // SAF providers may report -1 for unknown size; clamp so fingerprints stay comparable.
    let size = entry.size.max(0);
    // Provider mtimes arrive in seconds or ms; normalize to the fs path's millisecond domain.
    let mtime = norm_mtime(entry.mtime);

    // Stage dir beside the db file, mirroring the encrypt path's enc_tmp scratch placement.
    let stage_dir = state
        .db_path()
        .parent()
        .map(|p| p.join("nw_stage"))
        .unwrap_or_else(|| PathBuf::from("nw_stage"));

    let enqueued = reconcile_entry(
        state,
        watch,
        &rel_path,
        mtime,
        size,
        prefetched,
        // hash: stream the SAF document through blake3 via ContentResolver.
        {
            let doc_uri = doc_uri.clone();
            move || async move { crate::saf::hash_saf_document(doc_uri).await.map_err(AppError::Internal) }
        },
        // upload source: stage the SAF doc to a real fs path the encrypt/enqueue path can open.
        move || {
            let doc_uri = doc_uri.clone();
            let stage_dir = stage_dir.clone();
            async move {
                let staged = crate::saf::stage_saf_upload(doc_uri, stage_dir.to_string_lossy().to_string())
                    .await
                    .map_err(AppError::Internal)?;
                let path = PathBuf::from(&staged.path);
                // stage_saf_upload writes <stage_dir>/<uuid>/; drop that subdir once consumed.
                let cleanup_dir = path.parent().map(|p| p.to_path_buf());
                Ok(UploadSource { path, cleanup_dir })
            }
        },
    )
    .await?;
    // The enumerator supplied this entry's metadata, so a reconciled SAF entry is always seen.
    Ok(Reconciled { enqueued, seen: true })
}

/// Upload source for the shared reconcile tail. `cleanup_dir` (SAF staging only; desktop
/// passes `None`) is a scratch dir removed once the source is no longer needed.
struct UploadSource {
    path: PathBuf,
    cleanup_dir: Option<PathBuf>,
}

/// Shared reconcile tail for the fs (desktop) and SAF (Android) paths: mtime+size fast-path
/// skip, then hash + [`decide`]. `hash_fn` runs only on a miss; `source_fn` only once uploading.
#[allow(clippy::too_many_arguments)]
async fn reconcile_entry<S, HFut, SFut, HF, SF>(
    state: &S,
    watch: &NightWatch,
    rel_path: &str,
    mtime: i64,
    size: i64,
    prefetched: Option<&std::collections::HashMap<String, FileState>>,
    hash_fn: HF,
    source_fn: SF,
) -> AppResult<bool>
where
    S: NwCtx,
    HF: FnOnce() -> HFut,
    HFut: std::future::Future<Output = AppResult<String>>,
    SF: FnOnce() -> SFut,
    SFut: std::future::Future<Output = AppResult<UploadSource>>,
{
    // State from the scan's prefetched map when present; watcher path/tests pass None (DB hit).
    let prev = match prefetched {
        Some(map) => map.get(rel_path).cloned(),
        None => state.db().file_state_get(&watch.id, rel_path).await?,
    };
    if let Some(p) = &prev {
        if norm_mtime(p.mtime) == mtime && p.size == size {
            debug!(watch = %watch.id, rel = %rel_path, "night watcher: unchanged (mtime+size)");
            return Ok(false);
        }
    }

    // Backoff checked BEFORE hashing: a paused file mustn't pay a disk read + blake3 pass per
    // scan. When the pause elapses the row clears for fresh tries (TouchOnly refresh waits too).
    if let Some((fail_count, retry_after)) =
        state.db().file_retry_get(&watch.id, rel_path).await?
    {
        if fail_count >= MAX_UPLOAD_RETRIES {
            let now = chrono::Utc::now().timestamp();
            if retry_after > now {
                debug!(watch = %watch.id, rel = %rel_path, retry_after, "night watcher: upload paused after repeated failures");
                return Ok(false);
            }
            state.db().file_retry_clear(&watch.id, rel_path).await?;
        }
    }

    let hash = hash_fn().await?;
    match decide(prev.as_ref(), mtime, size, &hash) {
        Decision::Skip => {
            debug!(watch = %watch.id, rel = %rel_path, "night watcher: unchanged (hash)");
            return Ok(false);
        }
        Decision::TouchOnly => {
            // Content identical (touch/metadata-only): refresh fingerprint so we don't rehash next scan.
            let synced_etag = prev.and_then(|p| p.synced_etag);
            state
                .db()
                .file_state_upsert(
                    &watch.id,
                    FileState {
                        rel_path: rel_path.to_string(),
                        hash,
                        mtime,
                        size,
                        synced_etag,
                    },
                )
                .await?;
            debug!(watch = %watch.id, rel = %rel_path, "night watcher: content unchanged, fingerprint refreshed");
            return Ok(false);
        }
        Decision::Upload => {}
    }

    // Changed/new: claim so the watcher and full scan can't double-enqueue.
    if !state.nw_claim(&watch.id, rel_path) {
        debug!(watch = %watch.id, rel = %rel_path, "night watcher: already in flight, skipping");
        return Ok(false);
    }
    info!(watch = %watch.id, rel = %rel_path, size, "night watcher: enqueuing upload");

    let source = match source_fn().await {
        Ok(s) => s,
        Err(e) => {
            state.nw_unclaim(&watch.id, rel_path);
            return Err(e);
        }
    };

    // The SAF staging dir must outlive enqueue (the worker opens the file asynchronously), so
    // removal defers to the persist sink's terminal event; enqueue errors clean up here.
    let result = enqueue_upload_for(
        state,
        watch,
        rel_path,
        &source.path,
        mtime,
        size,
        hash,
        source.cleanup_dir.clone(),
    )
    .await;
    if result.is_err() {
        state.nw_unclaim(&watch.id, rel_path);
        if let Some(dir) = source.cleanup_dir {
            let _ = tokio::fs::remove_dir_all(&dir).await;
        }
    }
    result.map(|_| true)
}

/// Test-only reconcile entrypoint driving the real path via an injected [`NwCtx`].
#[cfg(feature = "nw-test-hooks")]
pub async fn reconcile_file_for_test<S: NwCtx>(
    state: &S,
    watch: &NightWatch,
    rel_path: &str,
    abs: &Path,
) -> AppResult<bool> {
    reconcile_file(state, watch, rel_path, abs, None)
        .await
        .map(|r| r.enqueued)
}

/// Test-only full-scan entrypoint.
#[cfg(feature = "nw-test-hooks")]
pub async fn reconcile_watch_for_test<S: NwCtx>(state: &S, watch: &NightWatch) -> AppResult<()> {
    let token = CancellationToken::new();
    reconcile_watch(state, watch, &token).await
}

/// Encrypt-if-needed, resolve the store, and enqueue the upload with a sink persisting
/// `nw_file_state` on completion. `stage_dir` (SAF only) is scratch the sink removes at term.
#[allow(clippy::too_many_arguments)]
async fn enqueue_upload_for<S: NwCtx>(
    state: &S,
    watch: &NightWatch,
    rel_path: &str,
    abs: &Path,
    mtime: i64,
    size: i64,
    hash: String,
    stage_dir: Option<PathBuf>,
) -> AppResult<()> {
    let mut opts = PutOptions::default();
    let (upload_path, cleanup_on_err) =
        crate::transfer::encrypt::encrypt_for_bucket_if_needed_with(
            state.db(),
            state.db_path(),
            &watch.account_id,
            &watch.bucket,
            abs,
            &mut opts,
        )
        .await?;

    let key = build_key(&watch.key_prefix, rel_path);
    let sink = persist_sink(
        state.clone(),
        watch.id.clone(),
        rel_path.to_string(),
        hash,
        mtime,
        size,
        stage_dir.clone(),
    );

    let enqueue = async {
        let store = state.store_for(&watch.account_id).await?;
        state
            .transfers()
            .enqueue_upload(
                store,
                watch.account_id.clone(),
                watch.bucket.clone(),
                key,
                upload_path,
                opts,
                sink,
                crate::db::transfers::TransferOrigin::NightWatch,
            )
            .await
    }
    .await;

    if enqueue.is_err() {
        if let Some(p) = cleanup_on_err {
            let _ = tokio::fs::remove_file(&p).await;
        }
    }
    enqueue.map(|_| ())
}

/// Progress sink: success records the synced fingerprint + clears the claim; failure/cancel
/// releases the claim for retry. `stage_dir` (SAF) removed on terminal events; worker holds it open.
#[allow(clippy::too_many_arguments)]
fn persist_sink<S: NwCtx>(
    state: S,
    watch_id: String,
    rel_path: String,
    hash: String,
    mtime: i64,
    size: i64,
    stage_dir: Option<PathBuf>,
) -> ProgressSink {
    let cleanup_stage = move || {
        if let Some(dir) = stage_dir.clone() {
            tokio::spawn(async move {
                let _ = tokio::fs::remove_dir_all(&dir).await;
            });
        }
    };
    ProgressSink::from_fn(move |event| match event {
        TransferEvent::Done { etag, .. } => {
            let state = state.clone();
            let watch_id = watch_id.clone();
            let rel_path = rel_path.clone();
            let hash = hash.clone();
            cleanup_stage();
            tokio::spawn(async move {
                let st = FileState {
                    rel_path: rel_path.clone(),
                    hash,
                    mtime,
                    size,
                    synced_etag: etag,
                };
                if let Err(e) = state.db().file_state_upsert(&watch_id, st).await {
                    warn!(watch = %watch_id, rel = %rel_path, "night watcher: persist synced state failed: {e}");
                } else {
                    info!(watch = %watch_id, rel = %rel_path, "night watcher: synced");
                }
                let _ = state.db().file_retry_clear(&watch_id, &rel_path).await;
                state.nw_unclaim(&watch_id, &rel_path);
            });
        }
        TransferEvent::Failed { error, .. } => {
            warn!(watch = %watch_id, rel = %rel_path, "night watcher: upload failed: {error}");
            cleanup_stage();
            let state = state.clone();
            let watch_id = watch_id.clone();
            let rel_path = rel_path.clone();
            tokio::spawn(async move {
                match state
                    .db()
                    .file_retry_record_failure(&watch_id, &rel_path, MAX_UPLOAD_RETRIES, RETRY_PAUSE_SECS)
                    .await
                {
                    Ok(n) if n >= MAX_UPLOAD_RETRIES => {
                        warn!(watch = %watch_id, rel = %rel_path, fails = n, pause_secs = RETRY_PAUSE_SECS, "night watcher: pausing file after repeated upload failures");
                    }
                    Ok(_) => {}
                    Err(e) => warn!(watch = %watch_id, rel = %rel_path, "night watcher: record retry failed: {e}"),
                }
                state.nw_unclaim(&watch_id, &rel_path);
            });
        }
        TransferEvent::Canceled { .. } => {
            debug!(watch = %watch_id, rel = %rel_path, "night watcher: upload canceled");
            cleanup_stage();
            state.nw_unclaim(&watch_id, &rel_path);
        }
        _ => {}
    })
}

/// Stream a file through blake3 on a blocking thread.
async fn hash_file(path: &Path) -> AppResult<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> AppResult<String> {
        let mut hasher = blake3::Hasher::new();
        let mut f = std::fs::File::open(&path)
            .map_err(|e| AppError::Internal(format!("open {}: {e}", path.display())))?;
        std::io::copy(&mut f, &mut hasher)
            .map_err(|e| AppError::Internal(format!("hash {}: {e}", path.display())))?;
        Ok(hasher.finalize().to_hex().to_string())
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
}

/// Recursively list files under `root` as `/`-separated rel paths + a subdir read-error count.
/// A root read failure is a hard error, so the caller stops rather than silently pruning state.
async fn collect_files(root: PathBuf) -> AppResult<(Vec<String>, u64)> {
    tokio::task::spawn_blocking(move || -> AppResult<(Vec<String>, u64)> {
        let mut out = Vec::new();
        let mut read_errors = 0u64;
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let rd = match std::fs::read_dir(&dir) {
                Ok(rd) => rd,
                Err(e) => {
                    if dir == root {
                        return Err(AppError::InvalidInput(format!(
                            "watched dir vanished during scan: {}: {e}",
                            dir.display()
                        )));
                    }
                    warn!(dir = %dir.display(), "read_dir failed: {e}");
                    read_errors += 1;
                    continue;
                }
            };
            for entry in rd.flatten() {
                let p = entry.path();
                match entry.file_type() {
                    Ok(t) if t.is_dir() => stack.push(p),
                    Ok(t) if t.is_file() => {
                        if let Ok(rel) = p.strip_prefix(&root) {
                            out.push(normalize_rel(rel));
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok((out, read_errors))
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
}

/// Pure reconcile decision core (unit-tested below).
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    Skip,
    TouchOnly,
    Upload,
}

/// Legacy rows stored whole seconds; ms epochs are ~1.7e12, so smaller positive values are
/// seconds and get scaled up on read. Misclassification costs at most one extra hash pass.
fn norm_mtime(v: i64) -> i64 {
    if v > 0 && v < 1_000_000_000_000 {
        v * 1000
    } else {
        v
    }
}

fn decide(prev: Option<&FileState>, mtime: i64, size: i64, hash: &str) -> Decision {
    match prev {
        Some(p) if norm_mtime(p.mtime) == mtime && p.size == size => Decision::Skip,
        Some(p) if p.hash == hash => Decision::TouchOnly,
        _ => Decision::Upload,
    }
}

fn normalize_rel(rel: &Path) -> String {
    let lossy = rel.to_string_lossy();
    // Windows: '\' → '/' for S3 keys + gitignore matching. On Unix a backslash is a legal
    // filename char, so converting there would collide onto unrelated keys.
    #[cfg(windows)]
    return lossy.replace('\\', "/");
    #[cfg(not(windows))]
    return lossy.into_owned();
}

fn build_key(prefix: &str, rel: &str) -> String {
    let p = prefix.trim_matches('/');
    if p.is_empty() {
        rel.to_string()
    } else {
        format!("{p}/{rel}")
    }
}

// Desktop uses the ripgrep `ignore` matcher; Android has no such dep, so it's a no-op there.

#[cfg(not(target_os = "android"))]
struct Matcher(Option<ignore::gitignore::Gitignore>);

#[cfg(not(target_os = "android"))]
impl Matcher {
    fn build(watch: &NightWatch) -> Self {
        let Some(path) = watch.ignore_file.as_ref() else {
            return Matcher(None);
        };
        let mut b = ignore::gitignore::GitignoreBuilder::new(&watch.local_dir);
        if let Some(e) = b.add(path) {
            warn!(watch = %watch.id, "ignore file load failed: {e}");
        }
        Matcher(b.build().ok())
    }

    fn is_ignored(&self, rel: &str) -> bool {
        match &self.0 {
            // Ancestors too, so a `build/` rule excludes `build/x.bin` (we filter per-file).
            Some(ig) => ig.matched_path_or_any_parents(rel, false).is_ignore(),
            None => false,
        }
    }
}

#[cfg(target_os = "android")]
struct Matcher;

#[cfg(target_os = "android")]
impl Matcher {
    fn build(_watch: &NightWatch) -> Self {
        Matcher
    }
    fn is_ignored(&self, _rel: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fs(mtime: i64, size: i64, hash: &str) -> FileState {
        FileState {
            rel_path: "a.txt".into(),
            hash: hash.into(),
            mtime,
            size,
            synced_etag: None,
        }
    }

    #[test]
    fn decide_skips_unchanged_by_fingerprint() {
        let prev = fs(1_700_000_000_000, 10, "aaa");
        assert_eq!(
            decide(Some(&prev), 1_700_000_000_000, 10, "zzz"),
            Decision::Skip
        );
    }

    #[test]
    fn decide_normalizes_legacy_second_mtime() {
        let prev = fs(1_700_000_000, 10, "aaa");
        assert_eq!(
            decide(Some(&prev), 1_700_000_000_000, 10, "zzz"),
            Decision::Skip
        );
    }

    #[test]
    fn decide_touch_only_when_bytes_match_but_fingerprint_drifts() {
        let prev = fs(1_700_000_000_000, 10, "aaa");
        assert_eq!(
            decide(Some(&prev), 1_700_000_100_000, 10, "aaa"),
            Decision::TouchOnly
        );
    }

    #[test]
    fn decide_uploads_new_and_changed() {
        assert_eq!(decide(None, 1_700_000_000_000, 10, "aaa"), Decision::Upload);
        let prev = fs(1_700_000_000_000, 10, "aaa");
        assert_eq!(
            decide(Some(&prev), 1_700_000_100_000, 20, "bbb"),
            Decision::Upload
        );
    }

    #[test]
    fn build_key_joins_prefix_and_normalizes_slashes() {
        assert_eq!(build_key("", "a/b.txt"), "a/b.txt");
        assert_eq!(build_key("photos", "a/b.txt"), "photos/a/b.txt");
        assert_eq!(build_key("/photos/", "a/b.txt"), "photos/a/b.txt");
        assert_eq!(normalize_rel(Path::new("a").join("b.txt").as_path()), "a/b.txt");
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn ignore_file_excludes_matched_paths() {
        use crate::db::night_watcher::NightWatch;

        let dir = tempfile::tempdir().unwrap();
        let ignore_path = dir.path().join(".cosmogignore");
        std::fs::write(&ignore_path, "*.log\nbuild/\n").unwrap();

        let watch = NightWatch {
            id: "w1".into(),
            account_id: "a1".into(),
            bucket: "b1".into(),
            local_dir: dir.path().to_string_lossy().to_string(),
            key_prefix: String::new(),
            ignore_file: Some(ignore_path.to_string_lossy().to_string()),
            delete_policy: "keep".into(),
            full_scan_secs: 300,
            enabled: true,
            last_scan_at: None,
            last_error: None,
            created_at: 0,
            tree_uri: None,
        };
        let m = Matcher::build(&watch);
        assert!(m.is_ignored("debug.log"));
        assert!(m.is_ignored("build/out.bin"));
        assert!(!m.is_ignored("keep.txt"));
    }
}

#[cfg(not(target_os = "android"))]
mod desktop {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use notify::{RecommendedWatcher, RecursiveMode, Watcher};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;
    use tracing::{info, warn};

    use super::{normalize_rel, reconcile_file, Matcher};
    use crate::state::AppState;

    /// Debounce window coalescing rapid FS events (e.g. editor atomic saves) before reconcile.
    const DEBOUNCE_MS: u64 = 750;
    /// How often live watcher handles resync with the DB.
    const RESYNC_SECS: u64 = 30;

    pub async fn run_watchers(state: AppState, token: CancellationToken) {
        let (tx, mut rx) = mpsc::unbounded_channel::<PathBuf>();
        let mut handles: HashMap<String, RecommendedWatcher> = HashMap::new();
        let mut pending: HashMap<PathBuf, Instant> = HashMap::new();
        let mut resync = tokio::time::interval(Duration::from_secs(RESYNC_SECS));
        let mut flush = tokio::time::interval(Duration::from_millis(DEBOUNCE_MS / 2));

        loop {
            tokio::select! {
                _ = token.cancelled() => {
                    info!("night watcher (desktop notify) stopped");
                    return;
                }
                _ = resync.tick() => {
                    sync_handles(&state, &mut handles, &tx).await;
                }
                Some(path) = rx.recv() => {
                    pending.insert(path, Instant::now());
                }
                _ = flush.tick() => {
                    if pending.is_empty() {
                        continue;
                    }
                    let ready: Vec<PathBuf> = pending
                        .iter()
                        .filter(|(_, t)| t.elapsed() >= Duration::from_millis(DEBOUNCE_MS))
                        .map(|(p, _)| p.clone())
                        .collect();
                    if ready.is_empty() {
                        continue;
                    }
                    for p in &ready {
                        pending.remove(p);
                    }
                    reconcile_paths(&state, ready).await;
                }
            }
        }
    }

    /// Add watchers for newly-enabled watches; drop ones gone or disabled.
    async fn sync_handles(
        state: &AppState,
        handles: &mut HashMap<String, RecommendedWatcher>,
        tx: &mpsc::UnboundedSender<PathBuf>,
    ) {
        let watches = match state.db.list_enabled_watches().await {
            Ok(w) => w,
            Err(e) => {
                warn!("night watcher: list watches failed: {e}");
                return;
            }
        };
        let live: std::collections::HashSet<String> =
            watches.iter().map(|w| w.id.clone()).collect();
        handles.retain(|id, _| live.contains(id));

        for w in watches {
            if handles.contains_key(&w.id) {
                continue;
            }
            let txc = tx.clone();
            let mut watcher = match notify::recommended_watcher(
                move |res: notify::Result<notify::Event>| {
                    if let Ok(ev) = res {
                        for p in ev.paths {
                            let _ = txc.send(p);
                        }
                    }
                },
            ) {
                Ok(w) => w,
                Err(e) => {
                    warn!(watch = %w.id, "create notify watcher failed: {e}");
                    continue;
                }
            };
            if let Err(e) = watcher.watch(Path::new(&w.local_dir), RecursiveMode::Recursive) {
                warn!(watch = %w.id, dir = %w.local_dir, "watch failed: {e}");
                continue;
            }
            info!(watch = %w.id, dir = %w.local_dir, "night watcher watching");
            handles.insert(w.id, watcher);
        }
    }

    async fn reconcile_paths(state: &AppState, paths: Vec<PathBuf>) {
        let watches = match state.db.list_enabled_watches().await {
            Ok(w) => w,
            Err(e) => {
                warn!("night watcher: list watches failed: {e}");
                return;
            }
        };
        for p in paths {
            for w in &watches {
                let root = Path::new(&w.local_dir);
                let Ok(rel_path) = p.strip_prefix(root) else {
                    continue;
                };
                let rel = normalize_rel(rel_path);
                if rel.is_empty() {
                    continue;
                }
                if Matcher::build(w).is_ignored(&rel) {
                    continue;
                }
                if let Err(e) = reconcile_file(state, w, &rel, &p, None).await {
                    warn!(watch = %w.id, rel = %rel, "reconcile failed: {e}");
                }
                break;
            }
        }
    }
}
