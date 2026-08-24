//! Input validation for Tauri command handlers. The app runs with full user privileges, so
//! obviously dangerous/ambiguous inputs are rejected here before reaching the SDK or filesystem.

use std::path::{Path, PathBuf};

use crate::error::{AppError, AppResult};

const MAX_FIELD_LEN: usize = 1024;

pub fn require_non_empty(field: &str, value: &str) -> AppResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(AppError::InvalidInput(format!("{field} must not be empty")));
    }
    if trimmed.len() > MAX_FIELD_LEN {
        return Err(AppError::InvalidInput(format!(
            "{field} exceeds maximum length of {MAX_FIELD_LEN}"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn expand_home(local_path: &str) -> String {
    if local_path.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return format!("{}/{}", home.to_string_lossy().trim_end_matches('/'), &local_path[2..]);
        }
    }
    local_path.to_string()
}

pub async fn validate_upload_source(local_path: &str) -> AppResult<PathBuf> {
    let expanded = expand_home(local_path);
    let path = Path::new(&expanded).to_path_buf();
    let path = path.as_path();
    if !path.is_absolute() {
        return Err(AppError::InvalidInput(
            "local_path must be absolute".into(),
        ));
    }
    let meta = tokio::fs::metadata(path).await
        .map_err(|e| AppError::InvalidInput(format!("local_path: {e}")))?;
    if !meta.is_file() {
        return Err(AppError::InvalidInput(
            "local_path must point to a regular file".into(),
        ));
    }
    Ok(path.to_path_buf())
}


/// Dest must be absolute with an existing parent dir (single-file rule; we create the file but not
/// arbitrary parent trees). Bulk directory downloads may create subdirs only inside the
/// user-supplied root — see `is_safe_relative_suffix` in bulk.rs.
pub async fn validate_download_dest(local_path: &str) -> AppResult<PathBuf> {
    let expanded = expand_home(local_path);
    let path_buf = Path::new(&expanded).to_path_buf();
    let path = path_buf.as_path();
    if !path.is_absolute() {
        return Err(AppError::InvalidInput(
            "local_path must be absolute".into(),
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        AppError::InvalidInput("local_path has no parent directory".into())
    })?;
    if !tokio::fs::metadata(parent).await.map(|m| m.is_dir()).unwrap_or(false) {
        return Err(AppError::InvalidInput(format!(
            "parent directory does not exist: {}",
            parent.display()
        )));
    }
    Ok(path.to_path_buf())
}
