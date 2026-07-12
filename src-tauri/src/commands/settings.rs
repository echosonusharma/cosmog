//! Tauri commands for application preferences.
//!
//! The FE typically calls [`get_settings`] once on startup, then
//! [`update_settings`] when the user changes anything. Updates use
//! merge-and-save semantics — fields not supplied in the patch keep their
//! previous values.

use serde::{Deserialize, Deserializer};
use tauri::State;

use crate::db::settings::AppSettings;
use crate::error::AppResult;
use crate::state::AppState;

// serde double-option: distinguishes "field absent" (None) from
// "field present and null" (Some(None)) — required so the FE can clear
// nullable settings by sending JSON null.
fn double_option<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Deserialize::deserialize(de).map(Some)
}

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
    #[serde(default, deserialize_with = "double_option")]
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
    #[serde(default, deserialize_with = "double_option")]
    pub http_proxy: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub custom_ca_path: Option<Option<String>>,
    pub request_log_ttl_days: Option<u32>,
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn update_settings(
    state: State<'_, AppState>,
    patch: SettingsPatch,
) -> AppResult<AppSettings> {
    let mut cur = state.load_settings().await?;
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
    if let Some(v) = patch.request_log_ttl_days {
        cur.request_log_ttl_days = v;
    }
    let saved = state.db.settings_save(cur).await?;
    state.invalidate_settings().await;
    if patch.transfer_concurrency.is_some() {
        state.set_transfer_concurrency(saved.transfer_concurrency as usize);
    }
    Ok(saved)
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn reset_settings(state: State<'_, AppState>) -> AppResult<AppSettings> {
    let s = state.db.settings_reset().await?;
    state.invalidate_settings().await;
    Ok(s)
}
