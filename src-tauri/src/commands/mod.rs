//! Tauri command handlers grouped by domain.
//!
//! Each submodule exposes `#[tauri::command]` functions that:
//! 1. Validate inputs (see [`crate::validate`]).
//! 2. Resolve the active provider client through [`crate::state::AppState::store_for`].
//! 3. Delegate the actual work to the [`crate::store::ObjectStore`] trait or
//!    the [`crate::transfer::TransferManager`].
//!
//! Errors propagate as [`crate::error::AppError`], which serializes to a stable
//! `{ code, message }` shape for the front-end.

pub mod accounts;
pub mod browse;
pub mod buckets;
pub mod bulk;
pub mod capabilities;
pub mod logs;
pub mod objects;
pub mod portable;
pub mod search;
pub mod settings;
pub mod transfers;
