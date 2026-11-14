//! Analysis phase: create the job, walk the tree off the runtime, persist items and aggregates.

use std::path::PathBuf;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::analysis::{FolderAnalysis, FolderAnalyzeEvent};
use super::walk::{walk, WalkMsg};
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
    let dest = keys::normalize_dest(dest_prefix)?;
    // The picked root itself may be a link: it is resolved once here and stored canonical;
    // everything below it is never followed.
    let unavailable = || AppError::FolderSourceUnavailable(keys::root_name(local_root));
    let canon = tokio::fs::canonicalize(local_root).await.map_err(|_| unavailable())?;
    let root: PathBuf = crate::transfer::source_guard::display_root(&canon);
    if !matches!(tokio::fs::metadata(&root).await.map(|m| m.is_dir()), Ok(true)) {
        return Err(unavailable());
    }
    let root_s = root.to_string_lossy().into_owned();
    if ctx.db().fu_find_unfinished(account_id, bucket, &root_s, &dest).await?.is_some() {
        return Err(AppError::FolderJobExists(keys::root_name(&root_s)));
    }
    let job_id = uuid::Uuid::new_v4().to_string();
    // Registered before the row exists, so startup cleanup can tell it belongs to this run.
    let rt = ctx.jobs().rt(&job_id);
    ctx.db()
        .fu_insert_job(NewJob {
            id: job_id.clone(),
            account_id: account_id.into(),
            bucket: bucket.into(),
            source_kind: "path".into(),
            source_root: root_s,
            dest_prefix: dest.clone(),
        })
        .await?;
    let cancel = CancellationToken::new();
    *rt.analysis_cancel.lock().unwrap() = Some(cancel.clone());
    rt.analysis_idle.send_replace(false);
    ctx.notify(&job_id, JobState::Analyzing);
    on_event(FolderAnalyzeEvent::Started { job_id: job_id.clone() });

    let res = run(ctx, &job_id, root, &dest, cancel, &on_event).await;
    *rt.analysis_cancel.lock().unwrap() = None;
    if res.is_err() {
        let _ = ctx.db().fu_delete_job(&job_id).await;
    }
    rt.analysis_idle.send_replace(true);
    match res {
        Ok(()) => view::get_analysis(ctx, &job_id).await,
        Err(e) => {
            ctx.jobs().forget(&job_id);
            ctx.notify(&job_id, JobState::Canceled);
            Err(e)
        }
    }
}

async fn run<C: FolderCtx>(
    ctx: &C,
    job_id: &str,
    root: PathBuf,
    dest: &str,
    cancel: CancellationToken,
    on_event: &(impl Fn(FolderAnalyzeEvent) + Send),
) -> AppResult<()> {
    let job = ctx.db().fu_get_job(job_id).await?;
    let encrypted = ctx.db().get_encryption_config(&job.account_id, &job.bucket).await?.is_some();
    let (tx, mut rx) = mpsc::channel::<WalkMsg>(8);
    let (root_c, dest_c, cancel_c) = (root.clone(), dest.to_string(), cancel.clone());
    let walker = tokio::task::spawn_blocking(move || walk(&root_c, &dest_c, &cancel_c, &tx));
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
