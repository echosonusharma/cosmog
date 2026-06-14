//! Tauri commands for application preferences.
//!
//! The FE typically calls [`get_settings`] once on startup, then
//! [`update_settings`] when the user changes anything. Updates use
//! merge-and-save semantics — fields not supplied in the patch keep their
//! previous values.

use serde::Deserialize;
use tauri::State;

use crate::db::settings::AppSettings;
use crate::error::AppResult;
use crate::state::AppState;

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> AppResult<AppSettings> {
    state.db.settings_load().await
}

/// Partial patch for [`AppSettings`]. Each `Option::Some` overrides the
/// corresponding field; `None` leaves it unchanged.
///
/// `default_download_dir` is `Option<Option<String>>` so the FE can
/// distinguish three cases: not supplied (`None`), set to a path
/// (`Some(Some(p))`), or explicitly cleared (`Some(None)`).
#[derive(Debug, Default, Deserialize)]
pub struct SettingsPatch {
    pub default_download_dir: Option<Option<String>>,
    pub transfer_concurrency: Option<u32>,
    pub multipart_parallelism: Option<u32>,
    pub multipart_threshold_bytes: Option<u64>,
    pub part_size_bytes: Option<u64>,
    pub prefix_sync_ttl_secs: Option<u64>,
    pub presign_default_expires_secs: Option<u64>,
    pub theme: Option<String>,
    pub show_hidden: Option<bool>,
    pub confirm_destructive: Option<bool>,
    pub http_proxy: Option<Option<String>>,
    pub custom_ca_path: Option<Option<String>>,
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn update_settings(
    state: State<'_, AppState>,
    patch: SettingsPatch,
) -> AppResult<AppSettings> {
    let mut cur = state.db.settings_load().await?;
    if let Some(v) = patch.default_download_dir {
        cur.default_download_dir = v;
    }
    if let Some(v) = patch.transfer_concurrency {
        cur.transfer_concurrency = v;
    }
    if let Some(v) = patch.multipart_parallelism {
        cur.multipart_parallelism = v;
    }
    if let Some(v) = patch.multipart_threshold_bytes {
        cur.multipart_threshold_bytes = v;
    }
    if let Some(v) = patch.part_size_bytes {
        cur.part_size_bytes = v;
    }
    if let Some(v) = patch.prefix_sync_ttl_secs {
        cur.prefix_sync_ttl_secs = v;
    }
    if let Some(v) = patch.presign_default_expires_secs {
        cur.presign_default_expires_secs = v;
    }
    if let Some(v) = patch.theme {
        cur.theme = v;
    }
    if let Some(v) = patch.show_hidden {
        cur.show_hidden = v;
    }
    if let Some(v) = patch.confirm_destructive {
        cur.confirm_destructive = v;
    }
    if let Some(v) = patch.http_proxy {
        cur.http_proxy = v;
    }
    if let Some(v) = patch.custom_ca_path {
        cur.custom_ca_path = v;
    }
    let saved = state.db.settings_save(cur).await?;
    if patch.transfer_concurrency.is_some() {
        state.set_transfer_concurrency(saved.transfer_concurrency as usize);
    }
    Ok(saved)
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn reset_settings(state: State<'_, AppState>) -> AppResult<AppSettings> {
    state.db.settings_reset().await
}
