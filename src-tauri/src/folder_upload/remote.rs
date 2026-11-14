//! Live destination check: a segmented listing of only the subtrees the folder touches.

use std::sync::Arc;

use serde::Serialize;
use tokio_util::sync::CancellationToken;

use super::{keys, FolderCtx};
use crate::db::folder_upload::{JobState, RemoteState};
use crate::error::{AppError, AppResult};
use crate::store::{ListOptions, ObjectStore};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FolderRemoteEvent {
    Progress { job_id: String, listed: i64 },
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderRemoteCheck {
    pub existing: i64,
    pub existing_bytes: i64,
    pub listed: i64,
    pub versioning: String,
    pub key_too_long: i64,
}

/// Sets the destination and checks it. A newer call for the same job cancels this one.
pub async fn check_remote<C: FolderCtx>(
    ctx: &C,
    job_id: &str,
    dest_prefix: &str,
    on_event: impl Fn(FolderRemoteEvent) + Send,
) -> AppResult<FolderRemoteCheck> {
    let dest = keys::normalize_dest(dest_prefix)?;
    let job = ctx.db().fu_get_job(job_id).await?;
    if !matches!(job.state, JobState::Analyzing | JobState::Analyzed) {
        return Err(AppError::InvalidInput("job is not awaiting approval".into()));
    }
    let rt = ctx.jobs().rt(job_id);
    let cancel = CancellationToken::new();
    if let Some(old) = rt.remote_cancel.lock().unwrap().replace(cancel.clone()) {
        old.cancel();
    }
    let key_too_long = ctx.db().fu_set_dest(job_id, &dest).await?;
    // Items still being inserted can't be matched: wait for the walk (it re-applies the dest at the end).
    let mut idle = rt.analysis_idle.subscribe();
    tokio::select! {
        _ = cancel.cancelled() => return Err(AppError::Canceled("remote check".into())),
        r = idle.wait_for(|v| *v) => { r.map_err(|_| AppError::Canceled("remote check".into()))?; }
    }
    let job = ctx.db().fu_get_job(job_id).await?;
    if job.state != JobState::Analyzed {
        return Err(AppError::Canceled("analysis discarded".into()));
    }
    let key_too_long = if job.dest_prefix == dest { ctx.db().fu_counts(job_id).await?.key_too_long } else { key_too_long };
    if cancel.is_cancelled() {
        return Err(AppError::Canceled("remote check".into()));
    }
    // Generation of this check: a newer check or dest change makes every later write a no-op.
    let Some(gen) = ctx.db().fu_begin_remote(job_id, &dest).await? else {
        return Err(AppError::Canceled("destination changed".into()));
    };
    let res = run(ctx, job_id, gen, &job.account_id, &job.bucket, &dest, &cancel, &on_event).await;
    match res {
        Ok((listed, versioning)) => {
            if cancel.is_cancelled() {
                return Err(AppError::Canceled("remote check".into()));
            }
            let (existing, existing_bytes) = ctx.db().fu_existing_totals(job_id).await?;
            let applied = ctx
                .db()
                .fu_finish_remote(job_id, gen, &dest, RemoteState::Done, existing, existing_bytes, Some(versioning.clone()))
                .await?;
            if !applied {
                return Err(AppError::Canceled("destination changed".into()));
            }
            Ok(FolderRemoteCheck { existing, existing_bytes, listed, versioning, key_too_long })
        }
        Err(e) => {
            if !matches!(e, AppError::Canceled(_)) {
                let _ = ctx.db().fu_finish_remote(job_id, gen, &dest, RemoteState::Failed, 0, 0, None).await;
            }
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run<C: FolderCtx>(
    ctx: &C,
    job_id: &str,
    gen: i64,
    account_id: &str,
    bucket: &str,
    dest: &str,
    cancel: &CancellationToken,
    on_event: &(impl Fn(FolderRemoteEvent) + Send),
) -> AppResult<(i64, String)> {
    let store = ctx.store_for(account_id).await?;
    let versioning = match store.get_bucket_versioning(bucket).await {
        Ok(true) => "enabled",
        Ok(false) => "disabled",
        Err(_) => "unknown",
    };
    let mut listed = 0i64;
    // Direct children, plus one entry per remote sub-prefix.
    let remote_dirs = list(ctx, &store, (job_id, gen), bucket, dest, dest, Some("/"), cancel, &mut listed, on_event).await?;
    for d in ctx.db().fu_top_dirs(job_id).await? {
        let sub = format!("{dest}{d}/");
        // Subtrees missing remotely need no listing.
        if remote_dirs.contains(&sub) {
            list(ctx, &store, (job_id, gen), bucket, dest, &sub, None, cancel, &mut listed, on_event).await?;
        }
    }
    Ok((listed, versioning.to_string()))
}

#[allow(clippy::too_many_arguments)]
async fn list<C: FolderCtx>(
    ctx: &C,
    store: &Arc<dyn ObjectStore>,
    (job_id, gen): (&str, i64),
    bucket: &str,
    dest: &str,
    prefix: &str,
    delimiter: Option<&str>,
    cancel: &CancellationToken,
    listed: &mut i64,
    on_event: &(impl Fn(FolderRemoteEvent) + Send),
) -> AppResult<std::collections::HashSet<String>> {
    let mut prefixes = std::collections::HashSet::new();
    let mut continuation = None;
    loop {
        let opts = ListOptions {
            prefix: (!prefix.is_empty()).then(|| prefix.to_string()),
            delimiter: delimiter.map(str::to_string),
            continuation: continuation.take(),
            max_keys: Some(1000),
        };
        let page = tokio::select! {
            _ = cancel.cancelled() => return Err(AppError::Canceled("remote check".into())),
            p = store.list_objects(bucket, opts) => p?,
        };
        *listed += page.objects.len() as i64;
        // A remote `x/` marker over a local dir is not an overwrite.
        let hits: Vec<(String, i64, Option<i64>)> = page
            .objects
            .iter()
            .filter(|o| !o.key.ends_with('/'))
            .filter_map(|o| o.key.strip_prefix(dest).map(|rel| (rel.to_string(), o.size, o.last_modified.map(|s| s * 1000))))
            .collect();
        if cancel.is_cancelled() {
            return Err(AppError::Canceled("remote check".into()));
        }
        ctx.db().fu_mark_remote(job_id, gen, hits).await?;
        prefixes.extend(page.prefixes);
        on_event(FolderRemoteEvent::Progress { job_id: job_id.into(), listed: *listed });
        match (page.is_truncated, page.continuation) {
            (true, Some(c)) => continuation = Some(c),
            // An incomplete listing would undercount what exists: fail rather than guess.
            (true, None) => return Err(AppError::S3("listing truncated without a continuation token".into())),
            _ => break,
        }
    }
    Ok(prefixes)
}
