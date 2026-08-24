//! Tauri commands managing Night Watcher watch definitions and status; the actual syncing
//! runs in the background task spawned by `crate::night_watcher::spawn`.

use serde::Serialize;
use tauri::State;

use crate::db::night_watcher::{NewWatch, NightWatch, WatchPatch};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::validate;

/// Floor on full-scan cadence: blocks a runaway 1s scan loop hammering S3 HEADs + CPU.
const MIN_FULL_SCAN_SECS: i64 = 30;

#[derive(Debug, Serialize)]
pub struct WatchStatus {
    pub id: String,
    pub enabled: bool,
    pub last_scan_at: Option<i64>,
    pub files_tracked: i64,
    pub last_error: Option<String>,
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn nw_list_watches(state: State<'_, AppState>) -> AppResult<Vec<NightWatch>> {
    state.db.list_watches().await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn nw_add_watch(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
    local_dir: String,
    key_prefix: Option<String>,
    ignore_file: Option<String>,
    full_scan_secs: i64,
    // FE forward-compat; the MVP honours only "keep", so it is not stored here.
    delete_policy: Option<String>,
    // Android SAF tree `content://` URI; when present, `local_dir` is only a human label.
    tree_uri: Option<String>,
) -> AppResult<NightWatch> {
    let _ = delete_policy;
    let account_id = validate::require_non_empty("account_id", &account_id)?;
    let bucket = validate::require_non_empty("bucket", &bucket)?;
    let local_dir = validate::require_non_empty("local_dir", &local_dir)?;

    state.db.get_account(&account_id).await?;

    let tree_uri = tree_uri.filter(|s| !s.trim().is_empty());

    // Tree mode skips fs validation: SAF URIs aren't filesystem paths.
    if tree_uri.is_none() {
        let dir = std::path::Path::new(&local_dir);
        if !dir.is_absolute() {
            return Err(AppError::InvalidInput("local_dir must be absolute".into()));
        }
        if !tokio::fs::metadata(&local_dir)
            .await
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            return Err(AppError::InvalidInput(format!(
                "local_dir is not an existing directory: {local_dir}"
            )));
        }
    }

    let ignore_file = ignore_file.filter(|s| !s.trim().is_empty());
    let full_scan_secs = full_scan_secs.max(MIN_FULL_SCAN_SECS);
    let id = uuid::Uuid::new_v4().to_string();
    state
        .db
        .insert_watch(
            &id,
            NewWatch {
                account_id,
                bucket,
                local_dir,
                key_prefix: key_prefix.unwrap_or_default(),
                ignore_file,
                full_scan_secs,
                tree_uri,
            },
        )
        .await?;

    let watch = state
        .db
        .get_watch(&id)
        .await?
        .ok_or_else(|| AppError::Internal("watch vanished after insert".into()))?;

    nw_refresh_service(&state).await;
    Ok(watch)
}

/// Launch the Android SAF tree picker; the desktop twin returns an error.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn nw_pick_tree(_state: State<'_, AppState>) -> AppResult<crate::saf::SafTree> {
    crate::saf::nw_pick_tree().await.map_err(|e| {
        if e == "canceled" {
            AppError::InvalidInput("tree pick canceled".into())
        } else {
            AppError::Internal(e)
        }
    })
}

/// Mirror enabled-watch presence onto the Android NightWatchService (foreground service +
/// boot flag); saf calls are no-ops on desktop. Helper, not a tauri command.
pub async fn nw_refresh_service(state: &AppState) {
    let active = !state
        .db
        .list_enabled_watches()
        .await
        .unwrap_or_default()
        .is_empty();
    let _ = crate::saf::set_nightwatch_service(active);
    let _ = crate::saf::set_nightwatch_boot_flag(active);

    // Desktop tray/close-guard/autostart mutate main-thread-only state (tray, macOS activation
    // policy) and this runs off-main, so hop threads.
    #[cfg(not(target_os = "android"))]
    {
        let app = state.app.clone();
        let _ = state.app.run_on_main_thread(move || {
            crate::app_lifecycle::apply(&app, active);
        });
    }
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn nw_update_watch(
    state: State<'_, AppState>,
    id: String,
    key_prefix: Option<String>,
    ignore_file: Option<String>,
    full_scan_secs: Option<i64>,
    delete_policy: Option<String>,
) -> AppResult<NightWatch> {
    let id = validate::require_non_empty("id", &id)?;
    state
        .db
        .update_watch(
            &id,
            WatchPatch {
                key_prefix,
                ignore_file,
                full_scan_secs: full_scan_secs.map(|s| s.max(MIN_FULL_SCAN_SECS)),
                delete_policy,
            },
        )
        .await?;
    state
        .db
        .get_watch(&id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("watch {id}")))
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn nw_delete_watch(state: State<'_, AppState>, id: String) -> AppResult<()> {
    let id = validate::require_non_empty("id", &id)?;
    state.db.delete_watch(&id).await?;
    nw_refresh_service(&state).await;
    Ok(())
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn nw_set_watch_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> AppResult<()> {
    let id = validate::require_non_empty("id", &id)?;
    state.db.set_watch_enabled(&id, enabled).await?;
    nw_refresh_service(&state).await;
    Ok(())
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn nw_get_status(state: State<'_, AppState>) -> AppResult<Vec<WatchStatus>> {
    let watches = state.db.list_watches().await?;
    let mut out = Vec::with_capacity(watches.len());
    for w in watches {
        let files_tracked = state.db.file_state_count(&w.id).await?;
        out.push(WatchStatus {
            id: w.id,
            enabled: w.enabled,
            last_scan_at: w.last_scan_at,
            files_tracked,
            last_error: w.last_error,
        });
    }
    Ok(out)
}
