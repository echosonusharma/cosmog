//! Where a job's files come from: a local path tree, or an Android SAF tree (`source_kind = saf`)
//! whose items carry their document URI. Also the SAF grant and foreground-service upkeep.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::engine::Run;
use super::{keys, FolderCtx};
use crate::db::folder_upload::{ItemRow, JobRow, JobState};
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::transfer::local_source;
use crate::saf::uri::same_tree;
use crate::transfer::source_guard::{inspect, Inspect};

pub(super) const KIND_PATH: &str = "path";
pub(super) const KIND_SAF: &str = "saf";

pub(super) fn is_saf(job: &JobRow) -> bool {
    job.source_kind == KIND_SAF
}

impl<C: FolderCtx> Run<C> {
    /// What the transfer opens: the file path, or the SAF document URI.
    pub(super) fn src(&self, item: &ItemRow) -> PathBuf {
        if is_saf(&self.job) {
            PathBuf::from(item.source_ref.clone().unwrap_or_default())
        } else {
            self.abs(&item.rel_path)
        }
    }

    /// Path sources: no-follow stat plus containment. SAF: fstat of a fresh fd (no links there).
    pub(super) async fn inspect_src(&self, src: &Path) -> Inspect {
        if !is_saf(&self.job) {
            return inspect(src, &self.canon_root).await;
        }
        if src.as_os_str().is_empty() {
            return Inspect::Missing;
        }
        match local_source::metadata(src).await {
            Ok(m) => Inspect::File(m),
            Err(e) => match e.kind() {
                std::io::ErrorKind::NotFound => Inspect::Missing,
                std::io::ErrorKind::InvalidInput => Inspect::Special,
                // Grant lost: pauses the job so the user can pick the folder again.
                std::io::ErrorKind::PermissionDenied => {
                    Inspect::Error(AppError::FolderSourceUnavailable(keys::root_name(&self.job.source_root)))
                }
                _ => Inspect::Error(AppError::Io(e.to_string())),
            },
        }
    }
}

/// How a recorded (size, mtime) is compared with the file now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StatRule {
    /// Local paths: size and mtime.
    Full,
    /// SAF local storage: listing and fstat mtime agree; a recorded 0 (none listed) is unknown.
    SafMtime,
    /// Other SAF providers may serve cache copies with a fresh mtime: size only.
    SizeOnly,
}

pub(super) fn stat_rule(job: &JobRow) -> StatRule {
    rule_for(&job.source_kind, &job.source_root)
}

fn rule_for(kind: &str, root: &str) -> StatRule {
    match (kind == KIND_SAF, crate::saf::uri::mtime_reliable(root)) {
        (false, _) => StatRule::Full,
        (true, true) => StatRule::SafMtime,
        (true, false) => StatRule::SizeOnly,
    }
}

/// Recorded (size, mtime) still describes the file.
pub(super) fn same_version(rule: StatRule, rec: (i64, i64), cur: (i64, i64)) -> bool {
    rec.0 == cur.0
        && match rule {
            StatRule::Full => rec.1 == cur.1,
            StatRule::SafMtime => rec.1 == 0 || rec.1 == cur.1,
            StatRule::SizeOnly => true,
        }
}

/// Root still usable: SAF tree answers a query, else the stored canonical path is still itself.
pub(super) async fn root_available(job: &JobRow) -> (bool, Option<PathBuf>) {
    if is_saf(job) {
        return (crate::saf::walk::tree_readable(job.source_root.clone()).await, None);
    }
    let root = Path::new(&job.source_root);
    let canon = tokio::fs::canonicalize(root).await.ok();
    // The stored root is canonical: resolving elsewhere now means a link in it was re-pointed.
    let same = canon.as_deref().is_some_and(|c| crate::transfer::source_guard::display_root(c) == root);
    let ok = same && matches!(tokio::fs::metadata(root).await, Ok(m) if m.is_dir()) && tokio::fs::read_dir(root).await.is_ok();
    (ok, canon)
}

/// SAF trees picked for folder upload whose job row does not exist yet (pick -> analyze).
static PICKS: Mutex<Vec<(String, Instant)>> = Mutex::new(Vec::new());
/// A pick never analyzed stops protecting its grant after this.
const PICK_TTL: Duration = Duration::from_secs(30 * 60);

pub fn pick_started(uri: &str) {
    let mut p = PICKS.lock().unwrap_or_else(|e| e.into_inner());
    p.retain(|(u, at)| at.elapsed() < PICK_TTL && !same_tree(u, uri));
    p.push((uri.to_string(), Instant::now()));
}

/// The job row exists (it holds the tree now) or the analysis failed.
pub fn pick_done(uri: &str) {
    PICKS.lock().unwrap_or_else(|e| e.into_inner()).retain(|(u, _)| !same_tree(u, uri));
}

fn pick_pending(uri: &str) -> bool {
    PICKS.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|(u, at)| at.elapsed() < PICK_TTL && same_tree(u, uri))
}

/// Job states that keep their tree: unfinished, or completed with errors (retry_failed reopens it).
fn holds_tree(state: JobState) -> bool {
    !state.is_finished() || state == JobState::CompletedWithErrors
}

/// A tree URI a job, a pending pick or a Night Watcher watch still reads.
pub async fn tree_in_use(db: &Db, uri: &str) -> AppResult<bool> {
    // A picker open right now may be persisting this very tree.
    if crate::saf::pick_in_progress() || pick_pending(uri) {
        return Ok(true);
    }
    let jobs = db.fu_list_jobs().await?;
    if jobs.iter().any(|j| j.source_kind == KIND_SAF && holds_tree(j.state) && same_tree(&j.source_root, uri)) {
        return Ok(true);
    }
    let watches = db.list_watches().await?;
    Ok(watches.iter().any(|w| w.tree_uri.as_deref().is_some_and(|t| same_tree(t, uri))))
}

/// Releases the persisted read grant once nothing uses the tree. Best effort.
pub async fn release_tree_if_unused(db: &Db, uri: &str) {
    if !crate::saf::is_content_uri(uri) {
        return;
    }
    match tree_in_use(db, uri).await {
        Ok(false) => {
            if let Err(e) = crate::saf::release_tree_permission(uri.to_string()).await {
                tracing::debug!("release tree permission: {e}");
            }
        }
        Ok(true) => {}
        Err(e) => tracing::warn!("tree grant check failed, kept: {e}"),
    }
}

/// A job ended or went away. Callers holding the scheduler lock pass `locked`, so a
/// retry_failed or resume cannot reopen the job between the state check and the release.
/// Never call with `locked: false` from code a lock holder may wait on (cancel waits for
/// analysis_idle). The lock is taken for path jobs too, so desktop tests exercise the ordering.
pub(super) async fn release_job_tree<C: FolderCtx>(ctx: &C, kind: &str, root: &str, locked: bool) {
    let jobs = ctx.jobs().clone();
    let _guard = if locked { None } else { Some(jobs.sched.lock().await) };
    if kind == KIND_SAF {
        release_tree_if_unused(ctx.db(), root).await;
    }
}

/// A pick that will not become a job (analyze refused it, wrong folder on re-pick).
pub async fn release_pick<C: FolderCtx>(ctx: &C, uri: &str) {
    if !crate::saf::is_content_uri(uri) {
        return;
    }
    pick_done(uri);
    release_job_tree(ctx, KIND_SAF, uri, false).await;
}

/// Startup: grants left by abandoned picks or a release the app died before. Every persisted
/// tree nothing holds is released.
pub(super) async fn sweep_grants(db: &Db) {
    match crate::saf::persisted_tree_uris().await {
        Ok(uris) => {
            for u in uris {
                release_tree_if_unused(db, &u).await;
            }
        }
        Err(e) => tracing::warn!("persisted grants list failed: {e}"),
    }
}

/// Folder runners hold TransferService alongside the UI. The flag flips now; the JNI call
/// runs off the async worker and applies whatever the flags are then, so order cannot invert it.
pub(super) fn hold_service(active: bool) {
    crate::saf::set_folder_service_hold(active);
    tokio::task::spawn_blocking(|| {
        if let Err(e) = crate::saf::apply_transfer_service_holds() {
            tracing::warn!("transfer service: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_pick_protects_its_tree_until_done() {
        let a = "content://com.android.externalstorage.documents/tree/primary%3APicks";
        let alt = "content://com.android.externalstorage.documents/tree/primary%3aPicks";
        assert!(!pick_pending(a));
        pick_started(alt);
        assert!(pick_pending(a), "normalized match");
        pick_done(a);
        assert!(!pick_pending(alt));
    }

    #[test]
    fn unknown_saf_mtime_is_not_a_change() {
        use StatRule::*;
        assert!(same_version(SafMtime, (5, 0), (5, 1234)));
        assert!(!same_version(Full, (5, 0), (5, 1234)), "desktop always compares mtime");
        assert!(!same_version(SafMtime, (5, 0), (6, 1234)), "size still decides");
        assert!(!same_version(SafMtime, (5, 10), (5, 11)), "a known local-storage mtime is compared");
        assert!(same_version(SizeOnly, (5, 10), (5, 11)), "cache-copy providers: mtime ignored");
        assert!(!same_version(SizeOnly, (5, 10), (4, 10)));
    }

    #[test]
    fn rule_follows_source_kind_and_provider() {
        assert_eq!(rule_for("path", "/a"), StatRule::Full);
        let ext = "content://com.android.externalstorage.documents/tree/primary%3AA";
        assert_eq!(rule_for("saf", ext), StatRule::SafMtime);
        assert_eq!(rule_for("saf", "content://com.google.android.apps.docs.storage/tree/x"), StatRule::SizeOnly);
    }

    #[test]
    fn completed_with_errors_keeps_the_tree_for_retry() {
        assert!(holds_tree(JobState::Paused) && holds_tree(JobState::Analyzed));
        assert!(holds_tree(JobState::CompletedWithErrors));
        assert!(!holds_tree(JobState::Completed) && !holds_tree(JobState::Canceled) && !holds_tree(JobState::Failed));
    }
}
