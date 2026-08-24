//! Diagnostic log access. The backend writes a daily-rolling log file to
//! `<app_data_dir>/logs/cosmog.log.YYYY-MM-DD`. These commands let the FE
//! show recent entries to the user and locate the directory for bug reports.

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

/// Return the path to the log directory.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub fn get_log_dir(state: State<'_, AppState>) -> AppResult<String> {
    Ok(state.log_dir.to_string_lossy().to_string())
}

/// Read the last `max_bytes` of today's log file. Returns empty string if no
/// log file exists yet (e.g. brand-new install where nothing has been logged).
/// `max_bytes` is clamped to a sensible upper bound to avoid loading huge
/// files into memory.
///
/// Fully async: the directory scan (stat + sort per entry) runs on a blocking
/// thread, and the tail read uses `tokio::fs` so neither stalls the runtime.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn get_log_tail(
    state: State<'_, AppState>,
    max_bytes: Option<u64>,
) -> AppResult<LogTail> {
    let cap = max_bytes.unwrap_or(256 * 1024).min(4 * 1024 * 1024);
    let dir = state.log_dir.clone();

    // Find the most recent rolling-suffix file. tracing-appender writes
    // `cosmog.log.YYYY-MM-DD`; we pick whichever has the latest mtime.
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
    // Tail-slice computation: seek to size-cap and read through EOF.
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
