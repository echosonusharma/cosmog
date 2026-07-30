//! Night Watcher: keep a local directory synced one-way to an S3 prefix.
//!
//! Two mechanisms feed the same reconcile core:
//!
//! - A periodic full scan (every watch, when `last_scan_at + full_scan_secs`
//!   has elapsed). This is the **source of truth**: it catches everything that
//!   changed while the app was closed, plus any FS events the watcher dropped.
//! - On desktop only, a `notify` filesystem watcher gives near-instant reaction
//!   to changes. It is a pure accelerator; correctness never depends on it.
//!   Android has no inotify under SAF, so it relies on the periodic scan.
//!
//! Change detection is a cheap `mtime + size` fast-path; only on a miss do we
//! hash the file (blake3) to decide whether the content actually changed. The
//! recorded state lives in `nw_file_state` (see `db::night_watcher`), kept
//! separate from the search cache so the two never corrupt each other.
//!
//! `delete_policy` is `"keep"` for the MVP: a locally-removed file just drops
//! its `nw_file_state` row; the remote object is left untouched.

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

/// The exact slice of `AppState` the reconcile core actually needs. Extracting
/// it behind a trait lets integration tests drive the *real* reconcile path
/// (change detection, encrypt-if-needed, upload, state persist) with an
/// injected MinIO store + `Db` + `TransferManager`, bypassing only the parts
/// that require a live `tauri::AppHandle` and the OS keyring. `AppState` is the
/// sole production implementor; its methods are called verbatim, so behavior is
/// unchanged.
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

/// How often the loop checks whether any watch is due for a full scan. The
/// per-watch cadence is governed by `full_scan_secs`, not this tick.
const TICK_SECS: u64 = 30;

/// Max consecutive upload failures for one file before it is paused.
const MAX_UPLOAD_RETRIES: i64 = 3;
/// How long a file is skipped after hitting [`MAX_UPLOAD_RETRIES`] failures.
const RETRY_PAUSE_SECS: i64 = 3600;

/// Spawn the Night Watcher. Returns immediately; the caller keeps the
/// [`CancellationToken`] alive for the process lifetime.
// Desktop only. On Android the loop runs in the :nightwatch service process
// (night_watcher_headless), never here, so this in-process spawn is gated off
// to keep a second reconcile loop from racing the service's writes.
#[cfg(not(target_os = "android"))]
pub fn spawn(state: AppState) -> CancellationToken {
    let cancel = CancellationToken::new();

    // Near-instant filesystem watcher task (accelerator; scan is source of truth).
    {
        let token = cancel.clone();
        let st = state.clone();
        tokio::spawn(async move { desktop::run_watchers(st, token).await });
    }

    let token = cancel.clone();
    tokio::spawn(run_loop(state, token));

    cancel
}

/// The periodic full-scan loop shared by every host. Generic over [`NwCtx`] so
/// the Android headless service process can drive the *same* reconcile core
/// with a lightweight (no `AppHandle`) context. Runs until the token cancels.
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
                if let Err(e) = run_once(&ctx).await {
                    warn!("night watcher tick failed: {e}");
                }
            }
        }
    }
}

/// RAII guard that releases a watch's full-scan claim on drop, so a panic or
/// early return in the scan task can never strand the claim.
struct ScanClaimGuard<S: NwCtx> {
    state: S,
    watch_id: String,
}

impl<S: NwCtx> Drop for ScanClaimGuard<S> {
    fn drop(&mut self) {
        self.state.nw_scan_unclaim(&self.watch_id);
    }
}

/// Trigger a full scan for every enabled watch whose interval has elapsed.
/// Each scan runs in its own task, guarded so the same watch never scans twice
/// concurrently.
async fn run_once<S: NwCtx>(state: &S) -> AppResult<()> {
    let watches = state.db().list_enabled_watches().await?;
    let now = Utc::now().timestamp();
    for w in watches {
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
        tokio::spawn(async move {
            // Drop-guard: releases the scan claim even if reconcile_watch panics
            // or the task is aborted, so a crashed scan never strands the watch.
            let _guard = ScanClaimGuard {
                state: state.clone(),
                watch_id: w.id.clone(),
            };
            let res = reconcile_watch(&state, &w).await;
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

/// Walk a watch's location and reconcile every (non-ignored) file. Errors on a
/// single file are swallowed + logged so one unreadable file can't abort the
/// whole scan.
///
/// Two modes, chosen at runtime by `watch.tree_uri`:
///
/// - `None` (desktop): `local_dir` is a real filesystem directory; walk it and
///   reconcile via [`reconcile_file`].
/// - `Some(uri)` (Android SAF): enumerate the tree via `crate::saf`, using the
///   provider-supplied mtime/size for the cheap fast-path, hashing/staging only
///   the entries that actually changed.
pub(crate) async fn reconcile_watch<S: NwCtx>(state: &S, watch: &NightWatch) -> AppResult<()> {
    if watch.tree_uri.is_some() {
        return reconcile_watch_saf(state, watch).await;
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
    let (mut scanned, mut ignored, mut enqueued, mut errors) = (0u64, 0u64, 0u64, read_errors);
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for rel in files {
        scanned += 1;
        if matcher.is_ignored(&rel) {
            ignored += 1;
            debug!(watch = %watch.id, rel = %rel, "night watcher: ignored");
            continue;
        }
        let abs = root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        match reconcile_file(state, watch, &rel, &abs).await {
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
    // Mark-and-sweep: this walk saw the whole tree, so any state row not in the
    // seen set is a file that no longer exists. delete_policy=keep means we only
    // drop the local state row (remote is untouched). Skipped only on a clean
    // walk (errors==0) so a transient read failure can't mass-prune.
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

/// Prune `nw_file_state` rows for files not seen in a completed full scan.
/// Returns the number of rows removed. delete_policy=keep: remote is untouched.
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
    let mut pruned = 0u64;
    for rel in known {
        if seen.contains(&rel) {
            continue;
        }
        if let Err(e) = state.db().file_state_delete(&watch.id, &rel).await {
            warn!(watch = %watch.id, rel = %rel, "night watcher: sweep delete failed: {e}");
        } else {
            pruned += 1;
            info!(watch = %watch.id, rel = %rel, "night watcher: local file gone, remote kept (delete_policy=keep)");
        }
    }
    pruned
}

/// SAF-tree variant of [`reconcile_watch`] (Android). Enumerates the tree via
/// `crate::saf::collect_tree_files`, then reconciles each entry using the
/// provider-supplied mtime/size fast-path. Only genuinely-changed entries are
/// hashed (`hash_saf_document`) and staged to a real fs path
/// (`stage_saf_upload`) before feeding the existing encrypt/enqueue path.
async fn reconcile_watch_saf<S: NwCtx>(state: &S, watch: &NightWatch) -> AppResult<()> {
    let uri = watch.tree_uri.clone().expect("tree_uri present");
    // read_errors seeds `errors` so an unreadable subtree blocks the sweep,
    // matching the desktop path. A revoked/deleted root still hard-errors above.
    let (entries, read_errors) = crate::saf::collect_tree_files(uri)
        .await
        .map_err(AppError::Internal)?;

    let matcher = Matcher::build(watch);
    let (mut scanned, mut ignored, mut enqueued, mut errors) = (0u64, 0u64, 0u64, read_errors);
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for entry in entries {
        if entry.is_dir {
            continue;
        }
        scanned += 1;
        if matcher.is_ignored(&entry.rel_path) {
            ignored += 1;
            debug!(watch = %watch.id, rel = %entry.rel_path, "night watcher: ignored");
            continue;
        }
        match reconcile_saf_entry(state, watch, &entry).await {
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
    // Mark-and-sweep: prune rows for tree entries no longer present. Only on a
    // clean enumeration+reconcile so a transient failure can't mass-prune.
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

/// Outcome of reconciling one entry. `enqueued` is true when an upload was
/// queued. `seen` is true when the entry was successfully observed this scan;
/// it is false only on a transient stat/metadata error, so the full-scan
/// mark-and-sweep does NOT prune a file we merely failed to read once.
pub(crate) struct Reconciled {
    enqueued: bool,
    seen: bool,
}

/// Reconcile a single filesystem file against its recorded state. Desktop path:
/// stat the file, then run the shared reconcile tail hashing/uploading straight
/// from the fs path. A transient stat error leaves the recorded state untouched.
pub(crate) async fn reconcile_file<S: NwCtx>(
    state: &S,
    watch: &NightWatch,
    rel_path: &str,
    abs: &Path,
) -> AppResult<Reconciled> {
    let meta = match tokio::fs::metadata(abs).await {
        Ok(m) => m,
        Err(e) => {
            // A stat failure can be transient (locked file, brief EIO). Do NOT
            // delete the state row: that would force a needless re-upload. Skip
            // the entry and leave it unseen so a genuine deletion still prunes
            // via mark-and-sweep once the file is truly gone.
            warn!(watch = %watch.id, rel = %rel_path, "night watcher: stat failed, skipping (state kept): {e}");
            return Ok(Reconciled { enqueued: false, seen: false });
        }
    };
    if !meta.is_file() {
        return Ok(Reconciled { enqueued: false, seen: false });
    }
    let size = meta.len() as i64;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let abs = abs.to_path_buf();
    let enqueued = reconcile_entry(
        state,
        watch,
        rel_path,
        mtime,
        size,
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

/// Reconcile a single SAF tree entry (Android). Mirrors [`reconcile_file`] but
/// feeds the provider-supplied mtime/size into the same shared tail, and only
/// hashes/stages the document when the tail decides an upload is required.
async fn reconcile_saf_entry<S: NwCtx>(
    state: &S,
    watch: &NightWatch,
    entry: &crate::saf::SafEntry,
) -> AppResult<Reconciled> {
    let rel_path = entry.rel_path.clone();
    let doc_uri = entry.doc_uri.clone();
    // SAF providers report -1 for unknown size; clamp to 0 so the fast-path
    // fingerprint stays a non-negative, comparable value.
    let size = entry.size.max(0);
    let mtime = entry.mtime;

    // Stage dir: a sibling of the db file, mirroring how the encrypt path uses
    // an enc_tmp scratch area under the same parent.
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
        // hash: stream the SAF document through blake3 via ContentResolver.
        {
            let doc_uri = doc_uri.clone();
            move || async move { crate::saf::hash_saf_document(doc_uri).await.map_err(AppError::Internal) }
        },
        // upload source: copy the SAF document to a real fs path first, so the
        // existing encrypt/enqueue path (which needs a File) can consume it.
        move || {
            let doc_uri = doc_uri.clone();
            let stage_dir = stage_dir.clone();
            async move {
                let staged = crate::saf::stage_saf_upload(doc_uri, stage_dir.to_string_lossy().to_string())
                    .await
                    .map_err(AppError::Internal)?;
                let path = PathBuf::from(&staged.path);
                // Clean up the per-call staging subdir after the upload consumes
                // the file. `stage_saf_upload` writes into <stage_dir>/<uuid>/.
                let cleanup_dir = path.parent().map(|p| p.to_path_buf());
                Ok(UploadSource { path, cleanup_dir })
            }
        },
    )
    .await?;
    // The SAF enumerator already reported this entry exists (it supplied the
    // mtime/size), so a reconciled SAF entry is always "seen".
    Ok(Reconciled { enqueued, seen: true })
}

/// The upload source produced for the shared reconcile tail: `path` is a real
/// filesystem file the encrypt/enqueue path can open; `cleanup_dir`, when set,
/// is a staging directory to remove once the source is no longer needed (SAF
/// staging only; desktop reuses the file in place).
struct UploadSource {
    path: PathBuf,
    cleanup_dir: Option<PathBuf>,
}

/// Shared reconcile tail for both the fs (desktop) and SAF (Android) paths.
///
/// Given a `rel_path` plus its cheap `mtime`/`size` fingerprint, it runs the
/// exact same logic the desktop path always did: fast-path skip on an unchanged
/// fingerprint, then hash + [`decide`], then TouchOnly-refresh or claim + enqueue.
///
/// `hash_fn` computes the content hash (only invoked on a fingerprint miss).
/// `source_fn` produces the upload source path (only invoked once the file is
/// claimed and definitely uploading), so SAF staging never touches unchanged
/// files.
#[allow(clippy::too_many_arguments)]
async fn reconcile_entry<S, HFut, SFut, HF, SF>(
    state: &S,
    watch: &NightWatch,
    rel_path: &str,
    mtime: i64,
    size: i64,
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
    let prev = state.db().file_state_get(&watch.id, rel_path).await?;
    // Fast path: unchanged by cheap fingerprint.
    if let Some(p) = &prev {
        if p.mtime == mtime && p.size == size {
            debug!(watch = %watch.id, rel = %rel_path, "night watcher: unchanged (mtime+size)");
            return Ok(false);
        }
    }

    let hash = hash_fn().await?;
    match decide(prev.as_ref(), mtime, size, &hash) {
        Decision::Skip => {
            debug!(watch = %watch.id, rel = %rel_path, "night watcher: unchanged (hash)");
            return Ok(false);
        }
        Decision::TouchOnly => {
            // Content identical (e.g. touch / metadata-only change): refresh
            // the cheap fingerprint so we don't rehash next scan. No upload.
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

    // Retry backoff: a file that failed to upload MAX_UPLOAD_RETRIES times is
    // paused for RETRY_PAUSE_SECS so a broken file/endpoint stops re-enqueuing
    // every scan. Once the pause elapses, clear the row for a fresh set of tries.
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

    // Content changed (or new). Claim the file so the watcher and the full
    // scan can't double-enqueue it.
    if !state.nw_claim(&watch.id, rel_path) {
        debug!(watch = %watch.id, rel = %rel_path, "night watcher: already in flight, skipping");
        return Ok(false);
    }
    info!(watch = %watch.id, rel = %rel_path, size, "night watcher: enqueuing upload");

    // Materialize the upload source only now that we are committed to uploading.
    let source = match source_fn().await {
        Ok(s) => s,
        Err(e) => {
            state.nw_unclaim(&watch.id, rel_path);
            return Err(e);
        }
    };

    // The staging scratch dir (SAF only) must outlive enqueue: the upload runs
    // later in a TransferManager worker that opens the file asynchronously. Its
    // removal is deferred to the persist sink, which fires on the terminal
    // transfer event. On an enqueue error we clean it up here.
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

/// Test-only reconcile entrypoint mirroring the production `AppState` path but
/// with an injected `NwCtx` (real `Db` + `TransferManager` + MinIO store). Not
/// compiled into release binaries.
#[cfg(feature = "nw-test-hooks")]
pub async fn reconcile_file_for_test<S: NwCtx>(
    state: &S,
    watch: &NightWatch,
    rel_path: &str,
    abs: &Path,
) -> AppResult<bool> {
    reconcile_file(state, watch, rel_path, abs)
        .await
        .map(|r| r.enqueued)
}

/// Test-only full-scan entrypoint (walks the dir, applies ignore, reconciles).
#[cfg(feature = "nw-test-hooks")]
pub async fn reconcile_watch_for_test<S: NwCtx>(state: &S, watch: &NightWatch) -> AppResult<()> {
    reconcile_watch(state, watch).await
}

/// Build the encrypted-if-needed source, resolve the store, and enqueue the
/// upload with a sink that persists `nw_file_state` on completion.
///
/// `stage_dir`, when `Some` (SAF only), is a per-file staging scratch dir that
/// the persist sink removes once the transfer reaches a terminal state.
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

/// A progress sink that, on a successful upload, records the file's synced
/// fingerprint and clears the in-flight claim. On failure/cancel it just
/// releases the claim so a later scan can retry.
///
/// `stage_dir` (SAF only) is the per-file staging scratch dir; it is removed on
/// every terminal event (done/failed/canceled) so a staged copy never lingers.
/// The upload worker holds the file open until the transfer finishes, so
/// removing it here is safe. Desktop passes `None` and nothing is touched.
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
                // Success clears any accumulated retry backoff for this file.
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

/// Stream a file through blake3. Runs on a blocking thread; O(64 KiB) memory.
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

/// Recursively list every regular file under `root`, returning paths relative
/// to `root` with `/` separators, plus a count of subdir read failures.
/// A read failure on `root` itself is a hard error (the watched dir vanished
/// mid-scan): return Err so the caller stops instead of silently pruning state.
/// Runs on a blocking thread.
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

/// What a reconcile should do with a file, given its recorded state and its
/// freshly-computed fingerprint. Pure: the decision core, unit-tested below.
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    /// Already in sync (same size + mtime). No hashing needed.
    Skip,
    /// Bytes unchanged but mtime/size fingerprint drifted: refresh record only.
    TouchOnly,
    /// New or genuinely changed content: upload.
    Upload,
}

fn decide(prev: Option<&FileState>, mtime: i64, size: i64, hash: &str) -> Decision {
    match prev {
        Some(p) if p.mtime == mtime && p.size == size => Decision::Skip,
        Some(p) if p.hash == hash => Decision::TouchOnly,
        _ => Decision::Upload,
    }
}

fn normalize_rel(rel: &Path) -> String {
    rel.to_string_lossy().replace('\\', "/")
}

fn build_key(prefix: &str, rel: &str) -> String {
    let p = prefix.trim_matches('/');
    if p.is_empty() {
        rel.to_string()
    } else {
        format!("{p}/{rel}")
    }
}

// ── ignore-file matcher (gitignore syntax) ──────────────────────────────────
// Desktop uses the ripgrep `ignore` matcher. Android has no `ignore` dep, so
// the matcher is a no-op there for the MVP.

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
            // Check ancestors too: a `build/` rule must exclude `build/x.bin`.
            // We walk every file then filter per-path, so directory-prune rules
            // only take effect via the parent check.
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
        let prev = fs(100, 10, "aaa");
        // Same mtime+size: skip without even trusting the hash.
        assert_eq!(decide(Some(&prev), 100, 10, "zzz"), Decision::Skip);
    }

    #[test]
    fn decide_touch_only_when_bytes_match_but_fingerprint_drifts() {
        let prev = fs(100, 10, "aaa");
        // mtime moved but content hash identical: refresh record, no upload.
        assert_eq!(decide(Some(&prev), 200, 10, "aaa"), Decision::TouchOnly);
    }

    #[test]
    fn decide_uploads_new_and_changed() {
        assert_eq!(decide(None, 100, 10, "aaa"), Decision::Upload);
        let prev = fs(100, 10, "aaa");
        assert_eq!(decide(Some(&prev), 200, 20, "bbb"), Decision::Upload);
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

// ── desktop notify watcher ──────────────────────────────────────────────────

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

    /// Debounce window: coalesce rapid events (editor atomic-save writes many
    /// events) before reconciling.
    const DEBOUNCE_MS: u64 = 750;
    /// How often to reconcile the live watcher-handle set with the DB.
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

    /// Add watchers for newly-enabled watches, drop watchers for ones that are
    /// gone or disabled.
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

    /// Map each changed absolute path back to its watch and reconcile it.
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
                if let Err(e) = reconcile_file(state, w, &rel, &p).await {
                    warn!(watch = %w.id, rel = %rel, "reconcile failed: {e}");
                }
                break;
            }
        }
    }
}
