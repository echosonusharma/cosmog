//! Folder upload commands (desktop UI; they compile everywhere). Thin shells over `folder_upload`.

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::State;

use crate::db::folder_upload::{ItemFilter, OverwritePolicy};
use crate::error::AppResult;
use crate::folder_upload::{
    self as fu, FolderAnalysis, FolderAnalyzeEvent, FolderItemPage, FolderJob, FolderRemoteCheck,
    FolderRemoteEvent,
};
use crate::state::AppState;
use crate::validate;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalPathKind {
    File,
    Dir,
    Other,
    Missing,
}

/// What each dropped path is; a dropped link is resolved since the user picked it.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn local_path_kinds(paths: Vec<String>) -> AppResult<Vec<LocalPathKind>> {
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        out.push(match tokio::fs::metadata(&p).await {
            Ok(m) if m.is_dir() => LocalPathKind::Dir,
            Ok(m) if m.is_file() => LocalPathKind::File,
            Ok(_) => LocalPathKind::Other,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => LocalPathKind::Missing,
            Err(_) => LocalPathKind::Other,
        });
    }
    Ok(out)
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn folder_upload_analyze(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    local_root: String,
    dest_prefix: String,
    on_event: Channel<FolderAnalyzeEvent>,
) -> AppResult<FolderAnalysis> {
    let account_id = validate::require_non_empty("account_id", &account_id)?;
    let bucket = validate::require_non_empty("bucket", &bucket)?;
    fu::analyze(&*state, &account_id, &bucket, &local_root, &dest_prefix, move |ev| {
        let _ = on_event.send(ev);
    })
    .await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn folder_upload_check_remote(
    state: State<'_, AppState>,
    job_id: String,
    dest_prefix: String,
    on_event: Channel<FolderRemoteEvent>,
) -> AppResult<FolderRemoteCheck> {
    fu::check_remote(&*state, &job_id, &dest_prefix, move |ev| {
        let _ = on_event.send(ev);
    })
    .await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn folder_upload_get_analysis(state: State<'_, AppState>, job_id: String) -> AppResult<FolderAnalysis> {
    fu::get_analysis(&*state, &job_id).await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn folder_upload_list_items(
    state: State<'_, AppState>,
    job_id: String,
    filter: ItemFilter,
    offset: i64,
    limit: i64,
) -> AppResult<FolderItemPage> {
    fu::list_items(&*state, &job_id, filter, offset, limit).await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn folder_upload_approve(
    state: State<'_, AppState>,
    job_id: String,
    overwrite_policy: Option<OverwritePolicy>,
    create_dir_markers: bool,
) -> AppResult<FolderJob> {
    fu::approve(&*state, &job_id, overwrite_policy, create_dir_markers).await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn folder_upload_list_jobs(state: State<'_, AppState>) -> AppResult<Vec<FolderJob>> {
    fu::list_jobs(&*state).await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn folder_upload_get_job(state: State<'_, AppState>, job_id: String) -> AppResult<FolderJob> {
    fu::get_job(&*state, &job_id).await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn folder_upload_pause(state: State<'_, AppState>, job_id: String) -> AppResult<()> {
    fu::pause(&*state, &job_id).await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn folder_upload_resume(state: State<'_, AppState>, job_id: String) -> AppResult<()> {
    fu::resume(&*state, &job_id).await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn folder_upload_cancel(state: State<'_, AppState>, job_id: String) -> AppResult<()> {
    fu::cancel(&*state, &job_id).await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn folder_upload_retry_failed(state: State<'_, AppState>, job_id: String) -> AppResult<i64> {
    fu::retry_failed(&*state, &job_id).await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn folder_upload_remove(state: State<'_, AppState>, job_id: String) -> AppResult<()> {
    fu::remove(&*state, &job_id).await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn folder_upload_clear_finished(state: State<'_, AppState>) -> AppResult<i64> {
    fu::clear_finished(&*state).await
}

/// True when closing the main window really quits the process (no tray keep-alive armed).
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn app_close_quits() -> AppResult<bool> {
    #[cfg(not(target_os = "android"))]
    {
        use crate::app_lifecycle as lc;
        Ok(!(lc::should_background() && !lc::quit_requested() && lc::tray_available()))
    }
    #[cfg(target_os = "android")]
    {
        Ok(false)
    }
}
