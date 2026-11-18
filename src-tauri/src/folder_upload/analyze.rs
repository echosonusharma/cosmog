//! Analysis phase: create the job, walk the tree off the runtime, persist items and aggregates.

use std::path::Path;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::analysis::{FolderAnalysis, FolderAnalyzeEvent};
use super::source::{pick_done, release_job_tree, release_pick, KIND_PATH, KIND_SAF};
use super::walk::{walk, WalkMsg};
use super::walk_saf::walk_saf;
use super::{keys, set_state, view, FolderCtx};
use crate::db::folder_upload::{JobState, NewJob};
use crate::error::{AppError, AppResult};

/// Walks `local_root` into a new `analyzed` job. Rejects `canceled` when the job is discarded mid-walk.
pub async fn analyze<C: FolderCtx>(
    ctx: &C,
    account_id: &str,
    bucket: &str,
    local_root: &str,
    dest_prefix: &str,
    on_event: impl Fn(FolderAnalyzeEvent) + Send,
) -> AppResult<FolderAnalysis> {
    // Refused before the job row exists: the pick has no owner, so its grant goes now.
    let refused = |e: AppError| async move {
        release_pick(ctx, local_root).await;
        Err(e)
    };
    let dest = match keys::normalize_dest(dest_prefix) {
        Ok(d) => d,
        Err(e) => return refused(e).await,
    };
    let (kind, root_s) = match source_root(local_root).await {
        Ok(v) => v,
        Err(e) => return refused(e).await,
    };
    match ctx.db().fu_find_unfinished(account_id, bucket, &root_s, &dest).await {
        Ok(None) => {}
        Ok(Some(_)) => return refused(AppError::FolderJobExists(keys::root_name(&root_s))).await,
        Err(e) => return refused(e).await,
    }
    let job_id = uuid::Uuid::new_v4().to_string();
    // Registered before the row exists, so startup cleanup can tell it belongs to this run.
    let rt = ctx.jobs().rt(&job_id);
    let inserted = ctx
        .db()
        .fu_insert_job(NewJob {
            id: job_id.clone(),
            account_id: account_id.into(),
            bucket: bucket.into(),
            source_kind: kind.into(),
            source_root: root_s.clone(),
            dest_prefix: dest.clone(),
        })
        .await;
    if let Err(e) = inserted {
        return refused(e).await;
    }
    // The job row holds the tree from here on.
    pick_done(&root_s);
    let cancel = CancellationToken::new();
    *rt.analysis_cancel.lock().unwrap() = Some(cancel.clone());
    rt.analysis_idle.send_replace(false);
    ctx.notify(&job_id, JobState::Analyzing);
    on_event(FolderAnalyzeEvent::Started { job_id: job_id.clone() });

    let res = run(ctx, &job_id, kind, root_s.clone(), &dest, cancel, &on_event).await;
    *rt.analysis_cancel.lock().unwrap() = None;
    if res.is_err() {
        let _ = ctx.db().fu_delete_job(&job_id).await;
    }
    // Before the release: cancel holds the scheduler lock while it waits for this.
    rt.analysis_idle.send_replace(true);
    if res.is_err() {
        release_job_tree(ctx, kind, &root_s, false).await;
    }
    match res {
        Ok(()) => view::get_analysis(ctx, &job_id).await,
        Err(e) => {
            ctx.jobs().forget(&job_id);
            ctx.notify(&job_id, JobState::Canceled);
            Err(e)
        }
    }
}

/// (source_kind, stored root): a SAF tree URI normalized and readable, or a canonical directory.
async fn source_root(local_root: &str) -> AppResult<(&'static str, String)> {
    let unavailable = || AppError::FolderSourceUnavailable(keys::root_name(local_root));
    if crate::saf::is_content_uri(local_root) {
        let uri = crate::saf::uri::normalize_tree_uri(local_root).ok_or_else(unavailable)?;
        if !crate::saf::walk::tree_readable(uri.clone()).await {
            return Err(unavailable());
        }
        return Ok((KIND_SAF, uri));
    }
    // The picked root itself may be a link: it is resolved once here and stored canonical;
    // everything below it is never followed.
    let canon = tokio::fs::canonicalize(local_root).await.map_err(|_| unavailable())?;
    let root = crate::transfer::source_guard::display_root(&canon);
    if !matches!(tokio::fs::metadata(&root).await.map(|m| m.is_dir()), Ok(true)) {
        return Err(unavailable());
    }
    Ok((KIND_PATH, root.to_string_lossy().into_owned()))
}

async fn run<C: FolderCtx>(
    ctx: &C,
    job_id: &str,
    kind: &str,
    root: String,
    dest: &str,
    cancel: CancellationToken,
    on_event: &(impl Fn(FolderAnalyzeEvent) + Send),
) -> AppResult<()> {
    let job = ctx.db().fu_get_job(job_id).await?;
    let encrypted = ctx.db().get_encryption_config(&job.account_id, &job.bucket).await?.is_some();
    let (tx, mut rx) = mpsc::channel::<WalkMsg>(8);
    let (dest_c, cancel_c, saf) = (dest.to_string(), cancel.clone(), kind == KIND_SAF);
    let walker = tokio::task::spawn_blocking(move || {
        if saf {
            walk_saf(&root, &dest_c, &cancel_c, &tx)
        } else {
            walk(Path::new(&root), &dest_c, &cancel_c, &tx)
        }
    });
    let mut insert_err = None;
    while let Some(msg) = rx.recv().await {
        match msg {
            WalkMsg::Items(items) => {
                if let Err(e) = ctx.db().fu_insert_items(job_id, items).await {
                    // Job deleted or DB failure: stop the walk.
                    cancel.cancel();
                    insert_err = Some(e);
                    break;
                }
            }
            WalkMsg::Progress { files, dirs, bytes, current_dir } => {
                on_event(FolderAnalyzeEvent::Progress { job_id: job_id.into(), files, dirs, bytes, current_dir });
            }
        }
    }
    drop(rx);
    let stats = walker.await.map_err(|e| AppError::Internal(format!("walk task: {e}")))?;
    if let Some(e) = insert_err {
        return Err(e);
    }
    let stats = stats?;
    if cancel.is_cancelled() {
        return Err(AppError::Canceled("analysis".into()));
    }
    let analysis = stats.finish(job_id, &job.source_root, dest, encrypted);
    let json = serde_json::to_string(&analysis).map_err(|e| AppError::Internal(e.to_string()))?;
    ctx.db().fu_set_analysis(job_id, json, analysis.files, analysis.total_bytes).await?;
    // The destination may have changed during the walk: re-evaluate key length for every item.
    let current = ctx.db().fu_get_job(job_id).await?.dest_prefix;
    ctx.db().fu_set_dest(job_id, &current).await?;
    set_state(ctx, job_id, JobState::Analyzed, None, None).await
}
