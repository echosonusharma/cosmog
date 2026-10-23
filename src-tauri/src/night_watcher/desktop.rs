use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::fs::{normalize_rel, Matcher};
use super::reconcile::reconcile_file;
use crate::state::AppState;

/// Coalesces rapid FS events (e.g. editor atomic saves).
const DEBOUNCE_MS: u64 = 750;
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
    // notify may report canonical paths (e.g. macOS /private/var) for a symlinked root.
    let roots: Vec<(PathBuf, Option<PathBuf>)> = watches
        .iter()
        .map(|w| {
            let root = PathBuf::from(&w.local_dir);
            let canon = std::fs::canonicalize(&root).ok().filter(|c| *c != root);
            (root, canon)
        })
        .collect();
    for p in paths {
        for (w, (root, canon)) in watches.iter().zip(&roots) {
            let rel_path = match p.strip_prefix(root) {
                Ok(r) => r,
                Err(_) => match canon.as_ref().and_then(|c| p.strip_prefix(c).ok()) {
                    Some(r) => r,
                    None => continue,
                },
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
