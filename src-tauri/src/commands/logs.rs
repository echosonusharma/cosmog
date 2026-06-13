//! Diagnostic log access. The backend writes a daily-rolling log file to
//! `<app_data_dir>/logs/cosmog.log.YYYY-MM-DD`. These commands let the FE
//! show recent entries to the user and locate the directory for bug reports.

use std::io::{Read, Seek, SeekFrom};

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
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub fn get_log_tail(
    state: State<'_, AppState>,
    max_bytes: Option<u64>,
) -> AppResult<LogTail> {
    let cap = max_bytes.unwrap_or(256 * 1024).min(4 * 1024 * 1024);
    let dir = &state.log_dir;
    // Find the most recent rolling-suffix file. tracing-appender writes
    // `cosmog.log.YYYY-MM-DD`; we pick whichever has the latest mtime.
    let mut candidates: Vec<_> = std::fs::read_dir(dir)
        .map_err(AppError::from)?
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
    let target = match candidates.into_iter().last() {
        Some(e) => e.path(),
        None => {
            return Ok(LogTail {
                path: dir.to_string_lossy().to_string(),
                bytes_read: 0,
                content: String::new(),
            });
        }
    };

    let mut file = std::fs::File::open(&target).map_err(AppError::from)?;
    let size = file.metadata().map_err(AppError::from)?.len();
    let offset = size.saturating_sub(cap);
    file.seek(SeekFrom::Start(offset)).map_err(AppError::from)?;
    let to_read = (size - offset) as usize;
    let mut buf = vec![0u8; to_read];
    file.read_exact(&mut buf).map_err(AppError::from)?;

    Ok(LogTail {
        path: target.to_string_lossy().to_string(),
        bytes_read: to_read as u64,
        content: String::from_utf8_lossy(&buf).to_string(),
    })
}
