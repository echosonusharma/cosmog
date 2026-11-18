//! Restart side: reconcile in-flight items with their transfer rows, startup cleanup, row purging.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use super::engine::Run;
use super::own::{owner, Owner};
use super::walk::mtime_ms;
use super::{control, set_state, FolderCtx};
use crate::db::folder_upload::{ItemRow, ItemState, JobRow, JobState};
use crate::db::transfers::{Transfer, TransferStatus};
use crate::error::AppResult;
use crate::store::ObjectStore;
use crate::transfer::source_guard::Inspect;

const KEEP_FINISHED: i64 = 50;
const FINISHED_TTL_SECS: i64 = 30 * 86_400;

/// Deletes rows, aborting their multipart upload first. A row whose abort failed keeps its
/// upload_id (the only record of it) for a later sweep.
pub(super) async fn abort_and_delete<C: FolderCtx>(ctx: &C, store: &Arc<dyn ObjectStore>, rows: Vec<Transfer>) -> AppResult<()> {
    let mut ids = Vec::new();
    for r in rows {
        if let Some(uid) = &r.upload_id {
            match store.abort_multipart_upload(&r.bucket, &r.key, uid).await {
                Ok(()) => {}
                Err(e) if e.is_not_found() => {}
                Err(e) => {
                    tracing::warn!(transfer = %r.id, "abort job multipart failed, row kept: {e}");
                    continue;
                }
            }
        }
        ids.push(r.id);
    }
    ctx.db().delete_transfer_rows(ids).await?;
    Ok(())
}

/// Aborts any multipart upload a job row still holds, then deletes every row of the job.
pub(super) async fn purge_rows<C: FolderCtx>(ctx: &C, store: &Arc<dyn ObjectStore>, job_id: &str) -> AppResult<()> {
    let rows = ctx.db().list_job_transfers(job_id).await?;
    for r in &rows {
        let _ = ctx.transfers().cancel(&r.id);
    }
    abort_and_delete(ctx, store, rows).await
}

/// ETag when the bucket holds this job's write of the item's recorded version. A canceled or
/// torn-down request may still have landed; only the bucket knows. None when unknown or absent.
pub(super) async fn landed(store: &Arc<dyn ObjectStore>, job: &JobRow, item: &ItemRow) -> Option<Option<String>> {
    let key = super::keys::object_key(&job.dest_prefix, &item.rel_path, false);
    match store.head_object(&job.bucket, &key).await {
        Ok(meta) if owner(&meta, &job.id, item) == (Owner::Ours { current: true }) => Some(meta.etag),
        Ok(_) => None,
        Err(e) => {
            if !e.is_not_found() {
                tracing::warn!(job = %job.id, "upload outcome check failed: {e}");
            }
            None
        }
    }
}

/// Cancel of a job with no live transfers: pending items end `canceled`; in-flight ones end
/// `done` if their write landed, else `canceled`.
pub(super) async fn cancel_cleanup<C: FolderCtx>(ctx: &C, store: &Arc<dyn ObjectStore>, job_id: &str) -> AppResult<()> {
    let db = ctx.db();
    db.fu_cancel_pending(job_id).await?;
    let job = db.fu_get_job(job_id).await?;
    for it in db.fu_inflight(job_id).await? {
        match landed(store, &job, &it).await {
            Some(etag) => db.fu_item_done(job_id, it.seq, etag).await?,
            None => db.fu_item_finished(job_id, it.seq, ItemState::Canceled, None, None).await?,
        }
    }
    purge_rows(ctx, store, job_id).await?;
    set_state(ctx, job_id, JobState::Canceled, None, None).await
}

async fn wait_settled<C: FolderCtx>(ctx: &C, id: &str) {
    ctx.transfers().suspend(id);
    for _ in 0..600 {
        if !ctx.transfers().is_live(id) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Matches `uploading` items to their rows: done rows finish the item, kept rows are resumed
/// (unencrypted multipart keeps its upload), a missing row is checked against the bucket first.
pub(super) async fn reconcile<C: FolderCtx>(run: &mut Run<C>) -> AppResult<()> {
    let job_id = run.job.id.clone();
    let ctx = run.ctx.clone();
    let rows: HashMap<String, Transfer> =
        ctx.db().list_job_transfers(&job_id).await?.into_iter().map(|r| (r.id.clone(), r)).collect();
    let mut used = HashSet::new();
    // Parked items (pending, still pointing at a kept row) are resumed by the runner later.
    for item in ctx.db().fu_select_pointing(&job_id).await? {
        if let Some(t) = &item.transfer_id {
            used.insert(t.clone());
        }
    }
    for item in ctx.db().fu_inflight(&job_id).await? {
        let Some(mut row) = item.transfer_id.as_ref().and_then(|t| rows.get(t)).cloned() else {
            verify_uploaded(run, &item).await?;
            continue;
        };
        used.insert(row.id.clone());
        if ctx.transfers().is_live(&row.id) {
            wait_settled(&ctx, &row.id).await;
            row = ctx.db().get_transfer(&row.id).await?;
        }
        if row.status == TransferStatus::Done {
            // Item first: a crash before the row delete must not re-upload.
            run.finish(item.seq, ItemState::Done, None).await?;
            ctx.db().delete_transfer_rows(vec![row.id]).await?;
            continue;
        }
        // Torn down mid-request (quit, crash): the write may have landed; never send it twice.
        if let Some(etag) = verified(run, &item).await {
            run.dirty = true;
            ctx.db().fu_item_done(&job_id, item.seq, etag).await?;
            abort_and_delete(&ctx, &run.store, vec![row]).await?;
            continue;
        }
        let Some(local) = run.pre_drift(&item).await? else {
            abort_and_delete(&ctx, &run.store, vec![row]).await?;
            continue;
        };
        if local.changed {
            abort_and_delete(&ctx, &run.store, vec![row]).await?;
            ctx.db().fu_item_retry_later(&job_id, item.seq, 0, None, false).await?;
            continue;
        }
        run.adopt(&item, row, local).await?;
    }
    // Rows no item points at (crash between enqueue and item update).
    let stray: Vec<Transfer> = rows.into_values().filter(|r| !used.contains(&r.id)).collect();
    for r in &stray {
        let _ = ctx.transfers().cancel(&r.id);
    }
    abort_and_delete(&ctx, &run.store, stray).await
}

/// `uploading` item whose row is gone (crash between the upload and the item update): done when
/// the bucket holds this job's upload of the current local version, else back to pending.
async fn verify_uploaded<C: FolderCtx>(run: &mut Run<C>, item: &ItemRow) -> AppResult<()> {
    let db = run.ctx.db().clone();
    if let Some(etag) = verified(run, item).await {
        run.dirty = true;
        return db.fu_item_done(&run.job.id, item.seq, etag).await;
    }
    db.fu_item_retry_later(&run.job.id, item.seq, 0, None, false).await
}

/// `landed`, and the local file is still the version that was written.
async fn verified<C: FolderCtx>(run: &Run<C>, item: &ItemRow) -> Option<Option<String>> {
    let etag = landed(&run.store, &run.job, item).await?;
    let local = run.inspect_src(&run.src(item)).await;
    let rule = super::source::stat_rule(&run.job);
    let unchanged = matches!(&local, Inspect::File(m) if super::source::same_version(rule, (item.size, item.mtime_ms), (m.len() as i64, mtime_ms(m))));
    unchanged.then_some(etag)
}

/// Startup, after orphan reaping: drop unapproved jobs, prune history, sweep orphan rows, resume.
pub async fn startup<C: FolderCtx>(ctx: &C) {
    let db = ctx.db();
    // Analyses started in this run (registered before their row exists) are kept.
    if let Err(e) = db.fu_startup_cleanup(ctx.jobs().ids(), ctx.jobs().started_at).await {
        tracing::warn!("folder upload cleanup failed: {e}");
    }
    if let Ok(jobs) = db.fu_list_jobs().await {
        for j in jobs.iter().filter(|j| j.state.is_finished()) {
            if let Ok(store) = ctx.store_for(&j.account_id).await {
                let _ = purge_rows(ctx, &store, &j.id).await;
            }
        }
    }
    let cutoff = chrono::Utc::now().timestamp() - FINISHED_TTL_SECS;
    if let Err(e) = db.fu_prune_finished(KEEP_FINISHED, cutoff).await {
        tracing::warn!("folder upload prune failed: {e}");
    }
    // After cleanup and prune: persisted grants nothing holds go (abandoned picks, a release
    // the app died before).
    super::source::sweep_grants(db).await;
    if let Err(e) = sweep_orphans(ctx).await {
        tracing::warn!("folder upload orphan sweep failed: {e}");
    }
    resume_all(ctx).await;
}

/// Job rows whose job is gone: abort their upload, delete them.
async fn sweep_orphans<C: FolderCtx>(ctx: &C) -> AppResult<()> {
    let mut by_account: HashMap<String, Vec<Transfer>> = HashMap::new();
    for r in ctx.db().list_orphan_job_transfers().await? {
        by_account.entry(r.account_id.clone()).or_default().push(r);
    }
    for (account, rows) in by_account {
        // No store, no abort: the rows (and their upload ids) wait for the next startup.
        match ctx.store_for(&account).await {
            Ok(store) => abort_and_delete(ctx, &store, rows).await?,
            Err(e) => tracing::warn!("orphan job rows kept, store unavailable: {e}"),
        }
    }
    Ok(())
}

/// Running and queued jobs continue (one at a time); paused jobs stay paused.
pub async fn resume_all<C: FolderCtx>(ctx: &C) {
    control::kick(ctx).await;
}
