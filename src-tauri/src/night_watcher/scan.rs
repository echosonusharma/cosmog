use std::path::PathBuf;

use chrono::Utc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::fs::{collect_files, Matcher};
use super::reconcile::{reconcile_file, reconcile_saf_entry};
use super::NwCtx;
use crate::db::night_watcher::NightWatch;
use crate::error::{AppError, AppResult};

/// Per-file errors are logged, never fatal.
pub(super) async fn reconcile_watch<S: NwCtx>(
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
    let scan_start = Utc::now().timestamp();
    // read_errors seeds `errors` so a subdir vanishing mid-scan blocks the sweep.
    let (files, read_errors) = collect_files(root.clone()).await?;
    let total = files.len();
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
    let pruned = finish_sweep(state, watch, &seen, errors, total, scan_start).await?;
    info!(
        watch = %watch.id,
        dir = %watch.local_dir,
        scanned, ignored, enqueued, errors, pruned,
        "night watcher: scan complete"
    );
    Ok(())
}

/// Sweeps only on a clean walk. An empty walk over a watch with state means unmounted/unreadable.
async fn finish_sweep<S: NwCtx>(
    state: &S,
    watch: &NightWatch,
    seen: &std::collections::HashSet<String>,
    errors: u64,
    total: usize,
    scan_start: i64,
) -> AppResult<u64> {
    if errors > 0 {
        return Ok(0);
    }
    if total == 0 && state.db().file_state_count(&watch.id).await? > 0 {
        return Err(AppError::InvalidInput(
            "watched folder is empty; skipped cleanup".into(),
        ));
    }
    Ok(sweep_deleted(state, watch, seen, scan_start).await)
}

/// Rows written after `scan_start` (by the watcher mid-scan) are kept. Remote is left untouched.
async fn sweep_deleted<S: NwCtx>(
    state: &S,
    watch: &NightWatch,
    seen: &std::collections::HashSet<String>,
    scan_start: i64,
) -> u64 {
    let known = match state.db().file_state_list_rel_paths(&watch.id, scan_start).await {
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
    match state
        .db()
        .file_state_delete_many(&watch.id, &doomed, scan_start)
        .await
    {
        Ok(n) => n,
        Err(e) => {
            warn!(watch = %watch.id, "night watcher: sweep delete failed: {e}");
            0
        }
    }
}

async fn reconcile_watch_saf<S: NwCtx>(
    state: &S,
    watch: &NightWatch,
    token: &CancellationToken,
) -> AppResult<()> {
    let uri = watch.tree_uri.clone().expect("tree_uri present");
    let scan_start = Utc::now().timestamp();
    // read_errors seeds `errors` (blocks sweep), matching the desktop path.
    let (entries, read_errors) = crate::saf::collect_tree_files(uri)
        .await
        .map_err(AppError::Internal)?;
    let total = entries.iter().filter(|e| !e.is_dir).count();

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
    let pruned = finish_sweep(state, watch, &seen, errors, total, scan_start).await?;
    info!(
        watch = %watch.id,
        tree = %watch.tree_uri.as_deref().unwrap_or(""),
        scanned, ignored, enqueued, errors, pruned,
        "night watcher: SAF scan complete"
    );
    Ok(())
}
