use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

use super::fs::hash_file;
use super::upload::{enqueue_upload_for, record_failure};
use super::{NwCtx, MAX_UPLOAD_RETRIES};
use crate::db::night_watcher::{FileState, NightWatch};
use crate::error::{AppError, AppResult};

/// `seen=false` lets mark-and-sweep prune the file's row.
pub(super) struct Reconciled {
    pub(super) enqueued: bool,
    pub(super) seen: bool,
}

pub(super) async fn reconcile_file<S: NwCtx>(
    state: &S,
    watch: &NightWatch,
    rel_path: &str,
    abs: &Path,
    prefetched: Option<&std::collections::HashMap<String, FileState>>,
) -> AppResult<Reconciled> {
    // Not following symlinks, matching collect_files.
    let meta = match tokio::fs::symlink_metadata(abs).await {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Genuinely gone: leave unseen so mark-and-sweep prunes the row.
            return Ok(Reconciled { enqueued: false, seen: false });
        }
        Err(e) => {
            // Transient (AV/indexer lock, EIO): error blocks the sweep; seen:false would force a re-upload.
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
        {
            let abs = abs.clone();
            move || async move { hash_file(&abs).await }
        },
        move || {
            let abs = abs.clone();
            async move { Ok(UploadSource { path: abs, cleanup_dir: None }) }
        },
    )
    .await?;
    // Metadata came from the enumerator, so a reconciled SAF entry is always seen.
    Ok(Reconciled { enqueued, seen: true })
}

pub(super) async fn reconcile_saf_entry<S: NwCtx>(
    state: &S,
    watch: &NightWatch,
    entry: &crate::saf::SafEntry,
    prefetched: Option<&std::collections::HashMap<String, FileState>>,
) -> AppResult<Reconciled> {
    let rel_path = entry.rel_path.clone();
    let doc_uri = entry.doc_uri.clone();
    let size = entry.size.max(0);
    // Unknown size/mtime: mtime 0 disables the fast path so the hash decides.
    let mtime = if entry.size < 0 { 0 } else { entry.mtime.max(0) };

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
        {
            let doc_uri = doc_uri.clone();
            move || async move { crate::saf::hash_saf_document(doc_uri).await.map_err(AppError::Internal) }
        },
        // Stage to a real fs path the encrypt/enqueue path can open.
        move || {
            let doc_uri = doc_uri.clone();
            let stage_dir = stage_dir.clone();
            async move {
                let staged = crate::saf::stage_saf_upload(doc_uri, stage_dir.to_string_lossy().to_string())
                    .await
                    .map_err(AppError::Internal)?;
                let path = PathBuf::from(&staged.path);
                // stage_saf_upload writes <stage_dir>/<uuid>/<file>; only that per-upload subdir is removed.
                let cleanup_dir = path.parent().map(|p| p.to_path_buf());
                Ok(UploadSource { path, cleanup_dir })
            }
        },
    )
    .await?;
    Ok(Reconciled { enqueued, seen: true })
}

/// `cleanup_dir` is SAF staging scratch, removed once the source is no longer needed.
struct UploadSource {
    path: PathBuf,
    cleanup_dir: Option<PathBuf>,
}

/// `hash_fn` runs only on a fast-path miss; `source_fn` only once uploading.
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
    let mut prev = match prefetched {
        Some(map) => map.get(rel_path).cloned(),
        None => state.db().file_state_get(&watch.id, rel_path).await?,
    };
    // The snapshot may predate a watcher-driven sync; re-read before hashing/uploading.
    if prefetched.is_some() && !prev.as_ref().is_some_and(|p| fast_match(p, mtime, size)) {
        prev = state.db().file_state_get(&watch.id, rel_path).await?;
    }
    if prev.as_ref().is_some_and(|p| fast_match(p, mtime, size)) {
        debug!(watch = %watch.id, rel = %rel_path, "night watcher: unchanged (mtime+size)");
        return Ok(false);
    }

    // Backoff before hashing so a paused file skips blake3; this also defers TouchOnly refresh.
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
            // Touch only: refresh fingerprint so we don't rehash next scan.
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

    // Claim so the watcher and full scan can't double-enqueue.
    if !state.nw_claim(&watch.id, rel_path) {
        debug!(watch = %watch.id, rel = %rel_path, "night watcher: already in flight, skipping");
        return Ok(false);
    }
    info!(watch = %watch.id, rel = %rel_path, size, "night watcher: enqueuing upload");

    let source = match source_fn().await {
        Ok(s) => s,
        Err(e) => {
            record_failure(state, &watch.id, rel_path).await;
            state.nw_unclaim(&watch.id, rel_path);
            return Err(e);
        }
    };

    // Staging dir must outlive enqueue (worker opens it async); the sink removes it, errors clean up here.
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
        record_failure(state, &watch.id, rel_path).await;
        state.nw_unclaim(&watch.id, rel_path);
        if let Some(dir) = source.cleanup_dir {
            let _ = tokio::fs::remove_dir_all(&dir).await;
        }
    }
    result.map(|_| true)
}

#[derive(Debug, PartialEq, Eq)]
enum Decision {
    Skip,
    TouchOnly,
    Upload,
}

/// mtimes are ms (legacy seconds rows miss once). mtime 0 means unknown, never trusted.
fn fast_match(p: &FileState, mtime: i64, size: i64) -> bool {
    mtime != 0 && p.mtime == mtime && p.size == size
}

fn decide(prev: Option<&FileState>, mtime: i64, size: i64, hash: &str) -> Decision {
    match prev {
        Some(p) if fast_match(p, mtime, size) => Decision::Skip,
        Some(p) if p.hash == hash => Decision::TouchOnly,
        _ => Decision::Upload,
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
    fn decide_compares_pre_2001_ms_mtime_as_is() {
        let prev = fs(900_000_000_000, 10, "aaa");
        assert_eq!(decide(Some(&prev), 900_000_000_000, 10, "zzz"), Decision::Skip);
        assert_eq!(decide(Some(&prev), 900_000_000_001, 10, "bbb"), Decision::Upload);
    }

    #[test]
    fn decide_never_trusts_unknown_mtime() {
        let prev = fs(0, 10, "aaa");
        assert_eq!(decide(Some(&prev), 0, 10, "bbb"), Decision::Upload);
        assert_eq!(decide(Some(&prev), 0, 10, "aaa"), Decision::TouchOnly);
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
}
