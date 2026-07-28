//! Tauri commands for Night Watcher: manage watch definitions and report
//! per-watch sync status. The actual syncing runs in the background task
//! spawned from `crate::night_watcher::spawn`.

use serde::Serialize;
use tauri::State;

use crate::db::night_watcher::{NewWatch, NightWatch, WatchPatch};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::validate;

/// Smallest allowed full-scan interval. Guards against a runaway 1s scan loop
/// hammering S3 HEADs and the CPU.
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
    // Accepted for forward-compat with the FE form; only "keep" is honoured in
    // the MVP so it is not stored from here.
    delete_policy: Option<String>,
    // Android only: a SAF tree `content://` URI. When present the watch syncs
    // that tree and `local_dir` is treated as a human label, not an fs path.
    tree_uri: Option<String>,
) -> AppResult<NightWatch> {
    let _ = delete_policy;
    let account_id = validate::require_non_empty("account_id", &account_id)?;
    let bucket = validate::require_non_empty("bucket", &bucket)?;
    // `local_dir` is required in both modes: on Android it is the human label
    // (e.g. the tree display name the FE passes).
    let local_dir = validate::require_non_empty("local_dir", &local_dir)?;

    // Confirm the account exists before creating a watch that references it.
    state.db.get_account(&account_id).await?;

    let tree_uri = tree_uri.filter(|s| !s.trim().is_empty());

    // SAF URIs are not filesystem paths, so skip the absolute-path/is_dir
    // validation entirely in tree mode. Desktop (no tree_uri) keeps the exact
    // existing filesystem validation.
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

/// Launch the Android SAF tree picker and return the picked tree
/// `{uri, display_name}`. On desktop the underlying saf twin returns an error.
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

/// Reflect the current enabled-watch count into the Android NightWatchService:
/// start/keep the foreground service (and set the boot flag) when at least one
/// watch is enabled, stop it otherwise. Both saf calls are no-ops on desktop.
/// Not a tauri command: callers invoke it after mutating watches.
pub async fn nw_refresh_service(state: &AppState) {
    let active = !state
        .db
        .list_enabled_watches()
        .await
        .unwrap_or_default()
        .is_empty();
    let _ = crate::saf::set_nightwatch_service(active);
    let _ = crate::saf::set_nightwatch_boot_flag(active);

    // Desktop: arm/disarm the tray + close-guard + autostart. apply() mutates
    // the tray and (on macOS) the activation policy, both main-thread only, and
    // this runs off the main thread (async command), so hop over.
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
