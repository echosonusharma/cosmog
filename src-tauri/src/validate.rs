//! Input-validation helpers used by Tauri command handlers.
//!
//! The desktop app runs with the user's full filesystem privileges, so we
//! cannot enforce hard sandboxing — but we *can* reject obviously dangerous or
//! ambiguous inputs (empty keys, relative paths, missing parent dirs) before
//! they reach the S3 SDK or the local filesystem.

use std::path::{Path, PathBuf};

use crate::error::{AppError, AppResult};

/// Reject empty or whitespace-only strings. Returns the trimmed value.
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

/// Validate an upload source path: must exist, be absolute, and resolve to a
/// regular file (not a directory or symlink to one).
fn expand_home(local_path: &str) -> String {
    if local_path.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return format!("{}/{}", home.to_string_lossy().trim_end_matches('/'), &local_path[2..]);
        }
    }
    local_path.to_string()
}

pub fn validate_upload_source(local_path: &str) -> AppResult<PathBuf> {
    let expanded = expand_home(local_path);
    let path = Path::new(&expanded).to_path_buf();
    let path = path.as_path();
    if !path.is_absolute() {
        return Err(AppError::InvalidInput(
            "local_path must be absolute".into(),
        ));
    }
    let meta = std::fs::metadata(path)
        .map_err(|e| AppError::InvalidInput(format!("local_path: {e}")))?;
    if !meta.is_file() {
        return Err(AppError::InvalidInput(
            "local_path must point to a regular file".into(),
        ));
    }
    Ok(path.to_path_buf())
}


/// Validate a download destination path: must be absolute and its parent
/// directory must exist (we will create the file itself, but refuse to create
/// arbitrary parent trees the user did not pick).
///
/// This is the single-file rule. The recursive
/// [`crate::bulk::download_directory`] command is separately permitted to
/// create subdirectories *inside the user-supplied `local_root`* and verifies
/// each resolved path stays within it — see `is_safe_relative_suffix` in
/// `bulk.rs`.
pub fn validate_download_dest(local_path: &str) -> AppResult<PathBuf> {
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
    if !parent.is_dir() {
        return Err(AppError::InvalidInput(format!(
            "parent directory does not exist: {}",
            parent.display()
        )));
    }
    Ok(path.to_path_buf())
}

