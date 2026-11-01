use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use notify::event::ModifyKind;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::fs::{collect_files, normalize_rel, Matcher};
use super::reconcile::reconcile_file;
use super::{forget_watcher, request_rescan, set_watcher_ok, NwCtx};
use crate::db::night_watcher::NightWatch;
use crate::state::AppState;

/// Coalesces rapid FS events (e.g. editor atomic saves).
const DEBOUNCE_MS: u64 = 750;
const RESYNC_SECS: u64 = 30;

pub async fn run_watchers(state: AppState, token: CancellationToken) {
    let (tx, mut rx) = mpsc::unbounded_channel::<(PathBuf, bool)>();
    let mut handles: HashMap<String, RecommendedWatcher> = HashMap::new();
    // path -> (last event, walk if it is a dir)
    let mut pending: HashMap<PathBuf, (Instant, bool)> = HashMap::new();
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
            Some((path, walk)) = rx.recv() => {
                let e = pending.entry(path).or_insert((Instant::now(), false));
                e.0 = Instant::now();
                e.1 |= walk;
            }
            _ = flush.tick() => {
                if pending.is_empty() {
                    continue;
                }
                let ready: Vec<(PathBuf, bool)> = pending
                    .iter()
                    .filter(|(_, (t, _))| t.elapsed() >= Duration::from_millis(DEBOUNCE_MS))
                    .map(|(p, (_, walk))| (p.clone(), *walk))
                    .collect();
                if ready.is_empty() {
                    continue;
                }
                for (p, _) in &ready {
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
    tx: &mpsc::UnboundedSender<(PathBuf, bool)>,
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
    handles.retain(|id, _| {
        let keep = live.contains(id);
        if !keep {
            forget_watcher(id);
        }
        keep
    });

    for w in watches {
        if handles.contains_key(&w.id) {
            continue;
        }
        let txc = tx.clone();
        let wid = w.id.clone();
        let mut watcher = match notify::recommended_watcher(
            move |res: notify::Result<notify::Event>| match res {
                // Overflow or coalesced events: individual paths were lost.
                Ok(ev) if ev.need_rescan() => {
                    warn!(watch = %wid, "notify requested rescan");
                    request_rescan(&wid);
                }
                Ok(ev) => {
                    // Windows reports a dir as modified when a child changes; only walk new dirs.
                    let walk = matches!(
                        ev.kind,
                        EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(_))
                    );
                    for p in ev.paths {
                        let _ = txc.send((p, walk));
                    }
                }
                // Coverage may be partial from here on. Rescan once, not on every repeat.
                Err(e) => {
                    if set_watcher_ok(&wid, false) {
                        warn!(watch = %wid, "notify error, falling back to periodic scan: {e}");
                        request_rescan(&wid);
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
        set_watcher_ok(&w.id, true);
        // Catch changes made while nothing was watching (app closed, watch disabled).
        request_rescan(&w.id);
        handles.insert(w.id, watcher);
    }
}

async fn reconcile_paths(state: &AppState, paths: Vec<(PathBuf, bool)>) {
    let watches = match state.db.list_enabled_watches().await {
        Ok(w) => w,
        Err(e) => {
            warn!("night watcher: list watches failed: {e}");
            return;
        }
    };
    // notify may report canonical paths (e.g. macOS /private/var) for a symlinked root.
    let roots: Vec<(PathBuf, Option<PathBuf>, Matcher)> = watches
        .iter()
        .map(|w| {
            let root = PathBuf::from(&w.local_dir);
            let canon = std::fs::canonicalize(&root).ok().filter(|c| *c != root);
            (root, canon, Matcher::build(w))
        })
        .collect();
    for (p, walk) in paths {
        for (w, (root, canon, matcher)) in watches.iter().zip(&roots) {
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
            if matcher.is_ignored(&rel) {
                continue;
            }
            if is_real_dir(&p) {
                if walk {
                    reconcile_dir(state, w, matcher, &rel, &p).await;
                }
            } else if let Err(e) = reconcile_file(state, w, &rel, &p, None).await {
                warn!(watch = %w.id, rel = %rel, "reconcile failed: {e}");
            }
            break;
        }
    }
}

/// Not following symlinks, matching collect_files.
fn is_real_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
}

/// A dir moved or copied in may arrive as one event for the dir alone, so walk it.
/// Returns how many uploads were enqueued.
pub(super) async fn reconcile_dir<S: NwCtx>(
    state: &S,
    w: &NightWatch,
    matcher: &Matcher,
    rel: &str,
    abs: &Path,
) -> u64 {
    let files = match collect_files(abs.to_path_buf()).await {
        Ok((files, _)) => files,
        Err(e) => {
            warn!(watch = %w.id, rel = %rel, "walk failed: {e}");
            return 0;
        }
    };
    let mut enqueued = 0;
    for sub in files {
        let child_rel = format!("{rel}/{sub}");
        if matcher.is_ignored(&child_rel) {
            continue;
        }
        let child = abs.join(sub.replace('/', std::path::MAIN_SEPARATOR_STR));
        match reconcile_file(state, w, &child_rel, &child, None).await {
            Ok(r) if r.enqueued => enqueued += 1,
            Ok(_) => {}
            Err(e) => warn!(watch = %w.id, rel = %child_rel, "reconcile failed: {e}"),
        }
    }
    enqueued
}
