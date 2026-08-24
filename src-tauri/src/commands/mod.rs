//! Tauri command handlers grouped by domain: validate inputs, resolve a client
//! via [`crate::state::AppState::store_for`], delegate; errors are `AppError`s.

pub mod accounts;
pub mod browse;
pub mod buckets;
pub mod bulk;
pub mod capabilities;
pub mod encryption;
pub mod logs;
#[cfg(not(target_os = "android"))]
pub mod mcp;
pub mod night_watcher;
pub mod objects;
pub mod portable;
pub mod request_logs;
pub mod search;
pub mod settings;
pub mod transfers;
