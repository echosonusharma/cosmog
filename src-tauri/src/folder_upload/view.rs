//! Wire shapes for the UI, built from job rows plus the runtime (cached counts, inflight files).

use std::time::{Duration, Instant};

use serde::Serialize;

use super::analysis::{warnings, FolderAnalysis};
use super::keys::object_key;
use super::runtime::{CachedCounts, JobRt};
use super::FolderCtx;
use crate::db::folder_upload::{
    Drift, ExcludeReason, ItemFilter, ItemKind, ItemRow, ItemState, JobRow, JobState, OverwritePolicy,
};
use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize)]
pub struct StateCounts {
    pub pending: i64,
    pub uploading: i64,
    /// Uploaded files only; empty-dir markers are not counted.
    pub done: i64,
    pub skipped_exists: i64,
    pub skipped_missing: i64,
    pub failed: i64,
    pub canceled: i64,
    pub excluded: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct InflightView {
    pub rel_path: String,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderJob {
    pub id: String,
    pub account_id: String,
    pub bucket: String,
    /// "path" or "saf" (`source_root` is then a tree URI).
    pub source_kind: String,
    pub source_root: String,
    /// Folder name to show: path basename or SAF tree folder name.
    pub display_name: String,
    pub dest_prefix: String,
    pub state: JobState,
    pub waiting: Option<&'static str>,
    pub pause_reason: Option<String>,
    pub overwrite_policy: Option<OverwritePolicy>,
    pub total_files: i64,
    pub total_bytes: i64,
    pub bytes_done: i64,
    pub counts: StateCounts,
    pub drift: i64,
    pub appeared_after_check: i64,
    pub inflight: Vec<InflightView>,
    pub error: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderItem {
    pub seq: i64,
    pub rel_path: String,
    pub key: String,
    pub kind: ItemKind,
    pub size: i64,
    pub mtime_ms: i64,
    pub state: ItemState,
    pub exclude_reason: Option<ExcludeReason>,
    pub link_target: Option<String>,
    pub remote_exists: bool,
    pub remote_size: Option<i64>,
    pub remote_mtime: Option<i64>,
    pub drift: Option<Drift>,
    pub error: Option<String>,
    pub attempts: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderItemPage {
    pub items: Vec<FolderItem>,
    pub total: i64,
}

/// Cached counts no older than `max_age`; refreshed from SQL otherwise.
pub(super) async fn counts<C: FolderCtx>(ctx: &C, rt: &JobRt, job_id: &str, max_age: Option<Duration>) -> AppResult<CachedCounts> {
    if let Some(c) = rt.counts.lock().unwrap().as_ref() {
        if max_age.map_or(true, |m| c.at.elapsed() < m) {
            return Ok(c.clone());
        }
    }
    let (counts, done_bytes) = ctx.db().fu_counts_and_bytes(job_id).await?;
    let fresh = CachedCounts { counts, done_bytes, at: Instant::now() };
    *rt.counts.lock().unwrap() = Some(fresh.clone());
    Ok(fresh)
}

async fn build<C: FolderCtx>(ctx: &C, row: JobRow) -> AppResult<FolderJob> {
    let rt = ctx.jobs().rt(&row.id);
    let c = counts(ctx, &rt, &row.id, None).await?;
    let inflight: Vec<InflightView> = rt
        .inflight
        .lock()
        .unwrap()
        .values()
        .map(|i| InflightView { rel_path: i.rel_path.clone(), bytes_done: i.bytes_done, bytes_total: i.bytes_total })
        .collect();
    let live: i64 = inflight.iter().map(|i| i.bytes_done as i64).sum();
    let waiting = if row.state == JobState::Running { *rt.waiting.lock().unwrap() } else { None };
    let k = c.counts;
    Ok(FolderJob {
        id: row.id,
        account_id: row.account_id,
        bucket: row.bucket,
        display_name: super::keys::display_name(&row.source_root),
        source_kind: row.source_kind,
        source_root: row.source_root,
        dest_prefix: row.dest_prefix,
        state: row.state,
        waiting,
        pause_reason: row.pause_reason,
        overwrite_policy: row.overwrite_policy,
        total_files: row.total_files,
        total_bytes: row.total_bytes,
        bytes_done: c.done_bytes + live,
        counts: StateCounts {
            pending: k.pending,
            uploading: k.uploading,
            done: k.done_files,
            skipped_exists: k.skipped_exists,
            skipped_missing: k.skipped_missing,
            failed: k.failed,
            canceled: k.canceled,
            excluded: k.excluded,
        },
        drift: k.drift,
        appeared_after_check: k.appeared_after_check,
        inflight,
        error: row.error,
        created_at: row.created_at,
        started_at: row.started_at,
        finished_at: row.finished_at,
        updated_at: row.updated_at,
    })
}

pub async fn get_job<C: FolderCtx>(ctx: &C, job_id: &str) -> AppResult<FolderJob> {
    let row = ctx.db().fu_get_job(job_id).await?;
    build(ctx, row).await
}

/// Approved jobs only; analyzing/analyzed ones belong to the open modal.
pub async fn list_jobs<C: FolderCtx>(ctx: &C) -> AppResult<Vec<FolderJob>> {
    let mut out = Vec::new();
    for row in ctx.db().fu_list_jobs().await? {
        if matches!(row.state, JobState::Analyzing | JobState::Analyzed) {
            continue;
        }
        out.push(build(ctx, row).await?);
    }
    Ok(out)
}

/// Stored aggregates with the destination-dependent figures refreshed.
pub async fn get_analysis<C: FolderCtx>(ctx: &C, job_id: &str) -> AppResult<FolderAnalysis> {
    let job = ctx.db().fu_get_job(job_id).await?;
    let raw = job.analysis_json.as_deref().ok_or_else(|| AppError::InvalidInput("analysis not finished".into()))?;
    let mut a: FolderAnalysis = serde_json::from_str(raw).map_err(|e| AppError::Internal(e.to_string()))?;
    let (files, bytes) = ctx.db().fu_pending_totals(job_id).await?;
    let c = ctx.db().fu_counts(job_id).await?;
    a.dest_prefix = job.dest_prefix;
    if !job.state.is_active() && !job.state.is_finished() {
        a.files = files;
        a.total_bytes = bytes;
    }
    a.excluded.insert(ExcludeReason::KeyTooLong.as_str().into(), c.key_too_long);
    a.warnings = warnings(&a);
    Ok(a)
}

pub async fn list_items<C: FolderCtx>(ctx: &C, job_id: &str, filter: ItemFilter, offset: i64, limit: i64) -> AppResult<FolderItemPage> {
    let job = ctx.db().fu_get_job(job_id).await?;
    let (rows, total) = ctx.db().fu_list_items(job_id, filter, offset.max(0), limit.clamp(1, 500)).await?;
    let items = rows.into_iter().map(|r| item(&job.dest_prefix, r)).collect();
    Ok(FolderItemPage { items, total })
}

fn item(dest: &str, r: ItemRow) -> FolderItem {
    FolderItem {
        key: object_key(dest, &r.rel_path, r.kind == ItemKind::Dir),
        seq: r.seq,
        rel_path: r.rel_path,
        kind: r.kind,
        size: r.size,
        mtime_ms: r.mtime_ms,
        state: r.state,
        exclude_reason: r.exclude_reason,
        link_target: r.link_target,
        remote_exists: r.remote_exists,
        remote_size: r.remote_size,
        remote_mtime: r.remote_mtime,
        drift: r.drift,
        error: r.error,
        attempts: r.attempts,
    }
}
