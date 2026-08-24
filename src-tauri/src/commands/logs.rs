//! Diagnostic log access: the backend writes a daily-rolling log to
//! `<app_data_dir>/logs/cosmog.log.YYYY-MM-DD`; the FE surfaces recent entries.

use std::path::PathBuf;

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Debug, serde::Serialize)]
pub struct LogTail {
    pub path: String,
    pub bytes_read: u64,
    pub content: String,
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub fn get_log_dir(state: State<'_, AppState>) -> AppResult<String> {
    Ok(state.log_dir.to_string_lossy().to_string())
}

/// Reads the last `max_bytes` (clamped) of today's log file; empty string if
/// none exists yet. Dir scan + tail read run off the async runtime.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn get_log_tail(
    state: State<'_, AppState>,
    max_bytes: Option<u64>,
) -> AppResult<LogTail> {
    let cap = max_bytes.unwrap_or(256 * 1024).min(4 * 1024 * 1024);
    let dir = state.log_dir.clone();

    // Pick the latest-mtime cosmog.log* file (tracing-appender daily rolls).
    let scan_dir = dir.clone();
    let target: Option<PathBuf> =
        tokio::task::spawn_blocking(move || -> std::io::Result<Option<PathBuf>> {
            let mut candidates: Vec<_> = std::fs::read_dir(&scan_dir)?
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .starts_with("cosmog.log")
                })
                .collect();
            candidates.sort_by_key(|e| {
                e.metadata()
                    .and_then(|m| m.modified())
                    .ok()
            });
            Ok(candidates.into_iter().last().map(|e| e.path()))
        })
        .await
        .map_err(|e| AppError::Internal(format!("log dir scan task failed: {e}")))?
        .map_err(AppError::from)?;

    let target = match target {
        Some(t) => t,
        None => {
            return Ok(LogTail {
                path: dir.to_string_lossy().to_string(),
                bytes_read: 0,
                content: String::new(),
            });
        }
    };

    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut file = tokio::fs::File::open(&target).await.map_err(AppError::from)?;
    let size = file.metadata().await.map_err(AppError::from)?.len();
    let offset = size.saturating_sub(cap);
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(AppError::from)?;
    let to_read = (size - offset) as usize;
    let mut buf = vec![0u8; to_read];
    file.read_exact(&mut buf).await.map_err(AppError::from)?;

    Ok(LogTail {
        path: target.to_string_lossy().to_string(),
        bytes_read: to_read as u64,
        content: String::from_utf8_lossy(&buf).to_string(),
    })
}
