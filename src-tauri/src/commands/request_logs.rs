use tauri::State;

use crate::db::request_logs::{RequestLog, RequestLogFilter};
use crate::error::AppResult;
use crate::state::AppState;

#[tauri::command]
pub async fn list_request_logs(
    state: State<'_, AppState>,
    limit: Option<u32>,
    offset: Option<u32>,
    search: Option<String>,
    status: Option<String>,
    operation: Option<String>,
) -> AppResult<Vec<RequestLog>> {
    state
        .db
        .list_request_logs(
            limit.unwrap_or(200),
            offset.unwrap_or(0),
            RequestLogFilter { search, status, operation },
        )
        .await
}

#[tauri::command]
pub async fn count_request_logs(
    state: State<'_, AppState>,
    search: Option<String>,
    status: Option<String>,
    operation: Option<String>,
) -> AppResult<i64> {
    state
        .db
        .count_request_logs(RequestLogFilter { search, status, operation })
        .await
}

#[tauri::command]
pub async fn clear_request_logs(state: State<'_, AppState>) -> AppResult<()> {
    state.db.clear_all_request_logs().await
}

#[tauri::command]
pub async fn purge_old_request_logs(state: State<'_, AppState>) -> AppResult<u64> {
    let settings = state.load_settings().await?;
    let cutoff =
        chrono::Utc::now().timestamp() - (settings.request_log_ttl_days as i64 * 86_400);
    state.db.delete_old_request_logs(cutoff).await
}
