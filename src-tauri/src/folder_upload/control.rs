//! User actions on jobs and the scheduler that keeps one job running at a time (FIFO).

use std::future::Future;
use std::pin::Pin;

use tokio::sync::watch;

use super::runtime::{RunnerHandle, Stop};
use super::resume as rs;
use super::{engine, set_state, view, FolderCtx, FolderJob};
use crate::db::folder_upload::{JobState, OverwritePolicy, RemoteState};
use crate::error::{AppError, AppResult};

/// Starts the running job, else the oldest queued one, unless a runner is live.
pub async fn kick<C: FolderCtx>(ctx: &C) {
    let jobs = ctx.jobs().clone();
    let _guard = jobs.sched.lock().await;
    kick_locked(ctx).await;
}

/// `kick` for callers already holding the scheduler lock.
async fn kick_locked<C: FolderCtx>(ctx: &C) {
    let jobs = ctx.jobs();
    if jobs.any_runner_live() {
        return;
    }
    let Ok(all) = ctx.db().fu_list_jobs().await else { return };
    let next = all
        .iter()
        .find(|j| j.state == JobState::Running)
        .or_else(|| all.iter().find(|j| j.state == JobState::Queued));
    let Some(job) = next else { return };
    if let Err(e) = set_state(ctx, &job.id, JobState::Running, None, None).await {
        tracing::warn!(job = %job.id, "folder upload start failed: {e}");
        return;
    }
    spawn_runner(ctx, &job.id);
}

// Boxed so the runner -> kick -> runner cycle has a nameable future type.
fn kick_boxed<C: FolderCtx>(ctx: C) -> Pin<Box<dyn Future<Output = ()> + Send>> {
    Box::pin(async move { kick(&ctx).await })
}

fn spawn_runner<C: FolderCtx>(ctx: &C, job_id: &str) {
    let rt = ctx.jobs().rt(job_id);
    let (stop_tx, stop_rx) = watch::channel(Stop::None);
    let (done_tx, done_rx) = watch::channel(false);
    *rt.runner.lock().unwrap() = Some(RunnerHandle { stop: stop_tx, done: done_rx });
    let (ctx, id) = (ctx.clone(), job_id.to_string());
    tokio::spawn(async move {
        engine::run_job(ctx.clone(), id, stop_rx).await;
        done_tx.send_replace(true);
        kick_boxed(ctx).await;
    });
}

/// Asks the live runner to stop and waits until it has settled; false when none is live.
async fn stop_runner<C: FolderCtx>(ctx: &C, job_id: &str, req: Stop) -> bool {
    let rt = ctx.jobs().rt(job_id);
    let done = {
        let guard = rt.runner.lock().unwrap();
        match guard.as_ref() {
            Some(h) if !*h.done.borrow() => {
                h.stop.send_replace(req);
                h.done.clone()
            }
            _ => return false,
        }
    };
    let mut done = done;
    let _ = done.wait_for(|d| *d).await;
    true
}

pub async fn approve<C: FolderCtx>(
    ctx: &C,
    job_id: &str,
    policy: Option<OverwritePolicy>,
    create_dir_markers: bool,
) -> AppResult<FolderJob> {
    let db = ctx.db();
    let job = db.fu_get_job(job_id).await?;
    if job.state != JobState::Analyzed {
        return Err(AppError::InvalidInput("job is not awaiting approval".into()));
    }
    if job.remote_state != RemoteState::Done || job.remote_prefix.as_deref() != Some(job.dest_prefix.as_str()) {
        return Err(AppError::InvalidInput("destination check not done for the current destination".into()));
    }
    if job.remote_existing > 0 && policy.is_none() {
        return Err(AppError::InvalidInput("overwrite policy required".into()));
    }
    if db.fu_find_unfinished(&job.account_id, &job.bucket, &job.source_root, &job.dest_prefix).await?.is_some() {
        return Err(AppError::FolderJobExists(super::keys::root_name(&job.source_root)));
    }
    let recipient = db.get_encryption_config(&job.account_id, &job.bucket).await?.map(|c| c.recipient);
    let (files, bytes) = db.fu_pending_totals(job_id).await?;
    db.fu_set_analysis(job_id, job.analysis_json.unwrap_or_default(), files, bytes).await?;
    db.fu_approve(job_id, policy, create_dir_markers, recipient, JobState::Queued).await?;
    ctx.notify(job_id, JobState::Queued);
    kick(ctx).await;
    view::get_job(ctx, job_id).await
}

// pause/resume/cancel hold the scheduler lock and read the state under it, so `kick` cannot
// start a job between the read and the write.
pub async fn pause<C: FolderCtx>(ctx: &C, job_id: &str) -> AppResult<()> {
    let jobs = ctx.jobs().clone();
    let _guard = jobs.sched.lock().await;
    let job = ctx.db().fu_get_job(job_id).await?;
    match job.state {
        JobState::Paused => Ok(()),
        JobState::Running if stop_runner(ctx, job_id, Stop::Pause).await => Ok(()),
        JobState::Running | JobState::Queued => set_state(ctx, job_id, JobState::Paused, Some("user".into()), None).await,
        _ => Err(AppError::InvalidInput("job cannot be paused".into())),
    }
}

/// A job paused for an encryption change continues under the current setting.
pub async fn resume<C: FolderCtx>(ctx: &C, job_id: &str) -> AppResult<()> {
    let jobs = ctx.jobs().clone();
    let _guard = jobs.sched.lock().await;
    let db = ctx.db();
    let job = db.fu_get_job(job_id).await?;
    match job.state {
        JobState::Queued | JobState::Running => return Ok(()),
        JobState::Paused => {}
        _ => return Err(AppError::InvalidInput("job is not paused".into())),
    }
    if job.pause_reason.as_deref() == Some("folder_encryption_changed") {
        let cur = db.get_encryption_config(&job.account_id, &job.bucket).await?.map(|c| c.recipient);
        db.fu_set_enc_recipient(job_id, cur).await?;
    }
    set_state(ctx, job_id, JobState::Queued, None, None).await?;
    kick_locked(ctx).await;
    Ok(())
}

/// Unapproved jobs are deleted; approved ones end `canceled` with uploaded objects left in place.
pub async fn cancel<C: FolderCtx>(ctx: &C, job_id: &str) -> AppResult<()> {
    let jobs = ctx.jobs().clone();
    let _guard = jobs.sched.lock().await;
    let job = match ctx.db().fu_get_job(job_id).await {
        Ok(j) => j,
        Err(e) if e.is_not_found() => return Ok(()),
        Err(e) => return Err(e),
    };
    let rt = ctx.jobs().rt(job_id);
    match job.state {
        JobState::Analyzing | JobState::Analyzed => {
            if let Some(t) = rt.remote_cancel.lock().unwrap().take() {
                t.cancel();
            }
            if let Some(t) = rt.analysis_cancel.lock().unwrap().clone() {
                t.cancel();
            }
            let _ = rt.analysis_idle.subscribe().wait_for(|v| *v).await;
            ctx.db().fu_delete_job(job_id).await?;
            ctx.jobs().forget(job_id);
            ctx.notify(job_id, JobState::Canceled);
            Ok(())
        }
        s if s.is_finished() => Ok(()),
        _ => {
            if stop_runner(ctx, job_id, Stop::Cancel).await {
                return Ok(());
            }
            let store = ctx.store_for(&job.account_id).await?;
            rs::cancel_cleanup(ctx, &store, job_id).await
        }
    }
}

/// Failed items go back to pending; a finished job reopens. Returns items reset.
pub async fn retry_failed<C: FolderCtx>(ctx: &C, job_id: &str) -> AppResult<i64> {
    let jobs = ctx.jobs().clone();
    let _guard = jobs.sched.lock().await;
    let job = ctx.db().fu_get_job(job_id).await?;
    if !job.state.is_active() && !matches!(job.state, JobState::Completed | JobState::CompletedWithErrors) {
        return Err(AppError::InvalidInput("job cannot retry".into()));
    }
    let n = ctx.db().fu_reset_failed(job_id).await?;
    ctx.jobs().rt(job_id).invalidate_counts();
    if n > 0 && job.state.is_finished() {
        set_state(ctx, job_id, JobState::Queued, None, None).await?;
        kick_locked(ctx).await;
    }
    Ok(n)
}

pub async fn remove<C: FolderCtx>(ctx: &C, job_id: &str) -> AppResult<()> {
    let job = ctx.db().fu_get_job(job_id).await?;
    if !job.state.is_finished() {
        return Err(AppError::InvalidInput("job is not finished".into()));
    }
    let store = ctx.store_for(&job.account_id).await?;
    rs::purge_rows(ctx, &store, job_id).await?;
    ctx.db().fu_delete_job(job_id).await?;
    ctx.jobs().forget(job_id);
    Ok(())
}

pub async fn clear_finished<C: FolderCtx>(ctx: &C) -> AppResult<i64> {
    let mut n = 0;
    for j in ctx.db().fu_list_jobs().await?.into_iter().filter(|j| j.state.is_finished()) {
        remove(ctx, &j.id).await?;
        n += 1;
    }
    Ok(n)
}

/// Before an account or bucket goes away: cancel its jobs so their multipart uploads are aborted.
pub async fn cancel_jobs_for<C: FolderCtx>(ctx: &C, account_id: &str, bucket: Option<&str>) {
    let Ok(jobs) = ctx.db().fu_list_jobs().await else { return };
    for j in jobs {
        if j.account_id != account_id || bucket.is_some_and(|b| b != j.bucket) || j.state.is_finished() {
            continue;
        }
        if let Err(e) = cancel(ctx, &j.id).await {
            tracing::warn!(job = %j.id, "cancel folder upload failed: {e}");
        }
    }
}
