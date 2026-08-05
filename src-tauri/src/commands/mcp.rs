//! MCP server config commands (desktop only).
//!
//! The FE reads and patches the config; each patch persists to settings and
//! restarts the listener so the running server reflects the new state.

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::error::AppResult;
use crate::state::AppState;

#[derive(Serialize)]
pub struct McpConfig {
    pub enabled: bool,
    pub port: u16,
    pub allow_write: bool,
    pub allow_delete: bool,
    pub bind_all_accounts: bool,
    pub acknowledged: bool,
    pub fs_root: Option<String>,
    pub token: String,
    pub running: bool,
    pub running_port: Option<u16>,
    pub url: String,
    pub advertised_tools: Vec<crate::mcp::AdvertisedTool>,
    pub accounts: Vec<McpAccount>,
}

#[derive(Serialize)]
pub struct McpAccount {
    pub id: String,
    pub name: String,
    /// False when the user turned this account off for MCP.
    pub enabled: bool,
}

async fn build_config(state: &AppState) -> AppResult<McpConfig> {
    let s = state.db.settings_load().await?;
    let token = crate::mcp::ensure_token().await?;
    let running_port = crate::mcp::running_port();
    let accounts = state
        .db
        .list_accounts()
        .await?
        .into_iter()
        .map(|a| McpAccount {
            enabled: !s.mcp_disabled_accounts.iter().any(|d| d == &a.id),
            id: a.id,
            name: a.name,
        })
        .collect();
    Ok(McpConfig {
        enabled: s.mcp_enabled,
        port: s.mcp_port,
        allow_write: s.mcp_allow_write,
        allow_delete: s.mcp_allow_delete,
        bind_all_accounts: s.mcp_bind_all_accounts,
        acknowledged: s.mcp_acknowledged,
        fs_root: s.mcp_fs_root.clone(),
        token,
        running: running_port.is_some(),
        running_port,
        url: format!("http://127.0.0.1:{}/mcp", s.mcp_port),
        advertised_tools: crate::mcp::advertised_tools(
            s.mcp_allow_write,
            s.mcp_allow_delete,
            &s.mcp_disabled_tools,
        ),
        accounts,
    })
}

#[derive(Deserialize)]
pub struct McpConfigPatch {
    pub enabled: Option<bool>,
    pub port: Option<u16>,
    pub allow_write: Option<bool>,
    pub allow_delete: Option<bool>,
    pub bind_all_accounts: Option<bool>,
    pub acknowledged: Option<bool>,
    pub disabled_tools: Option<Vec<String>>,
    pub disabled_accounts: Option<Vec<String>>,
    /// Absent = leave unchanged. A string sets the folder; an empty string
    /// clears it (normalize() collapses empty to None).
    pub fs_root: Option<String>,
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn mcp_get_config(state: State<'_, AppState>) -> AppResult<McpConfig> {
    build_config(&state).await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn mcp_set_config(
    state: State<'_, AppState>,
    patch: McpConfigPatch,
) -> AppResult<McpConfig> {
    let mut s = state.db.settings_load().await?;
    if let Some(v) = patch.enabled {
        s.mcp_enabled = v;
    }
    if let Some(v) = patch.port {
        s.mcp_port = v;
    }
    if let Some(v) = patch.allow_write {
        s.mcp_allow_write = v;
    }
    if let Some(v) = patch.allow_delete {
        s.mcp_allow_delete = v;
    }
    if let Some(v) = patch.bind_all_accounts {
        s.mcp_bind_all_accounts = v;
    }
    if let Some(v) = patch.acknowledged {
        s.mcp_acknowledged = v;
    }
    if let Some(v) = patch.disabled_tools {
        s.mcp_disabled_tools = v;
    }
    if let Some(v) = patch.disabled_accounts {
        s.mcp_disabled_accounts = v;
    }
    if let Some(v) = patch.fs_root {
        s.mcp_fs_root = Some(v);
    }
    state.db.settings_save(s).await?;
    state.invalidate_settings().await;
    // Restart (or stop) the listener so the change takes effect now. A bind
    // failure (e.g. port in use) surfaces to the FE.
    crate::mcp::apply(&state).await?;
    build_config(&state).await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn mcp_regenerate_token(state: State<'_, AppState>) -> AppResult<String> {
    let token = crate::mcp::regenerate_token().await?;
    // Restart so a running server picks up the new token.
    crate::mcp::apply(&state).await?;
    Ok(token)
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn mcp_status(state: State<'_, AppState>) -> AppResult<McpConfig> {
    build_config(&state).await
}
