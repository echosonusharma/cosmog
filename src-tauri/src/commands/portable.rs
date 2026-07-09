//! Backup / restore commands for moving Cosmog config between machines.
//!
//! Exports a JSON document containing accounts (sans secrets) and the user
//! settings row-set. Secrets stay in the OS keyring and are intentionally
//! never serialized — the receiving machine must re-enter them after import.
//!
//! `import_config` is a merge, not a replace: existing accounts with the same
//! `id` get their endpoint/region/access_key updated; new ones are inserted.
//! Settings are merged field-by-field.

use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::db::accounts::Account;
use crate::db::settings::AppSettings;
use crate::error::AppResult;
use crate::state::AppState;

/// Serialized snapshot of a Cosmog install. Versioned so future schema
/// changes can be detected and migrated on import.
#[derive(Debug, Serialize, Deserialize)]
pub struct ConfigExport {
    pub schema_version: u32,
    pub exported_at: i64,
    pub accounts: Vec<Account>,
    pub settings: AppSettings,
}

const EXPORT_SCHEMA: u32 = 1;

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn export_config(state: State<'_, AppState>) -> AppResult<ConfigExport> {
    let accounts = state.db.list_accounts().await?;
    let settings = state.load_settings().await?;
    Ok(ConfigExport {
        schema_version: EXPORT_SCHEMA,
        exported_at: Utc::now().timestamp(),
        accounts,
        settings,
    })
}

#[derive(Debug, Serialize)]
pub struct ImportSummary {
    pub accounts_inserted: usize,
    pub accounts_updated: usize,
    pub settings_applied: bool,
}

/// Copy the live SQLite file to `dest_path` after a WAL checkpoint. Useful as
/// a "Save backup..." command in the FE. Does NOT include OS-keyring secrets.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn backup_database(
    state: State<'_, AppState>,
    dest_path: String,
) -> AppResult<()> {
    let dest = std::path::PathBuf::from(dest_path);
    state.db.backup_to(dest).await
}

/// Stage a backup file to be applied on the next app launch.
///
/// We cannot replace the live SQLite file while the process holds open
/// connections to it. Instead we write the source bytes to
/// `<db_path>.restore_pending`; on next startup the boot code atomically
/// renames it over the live DB after one final SQLite-header sanity check.
///
/// The source is validated before staging: must be an existing regular file,
/// non-empty, and start with the SQLite 3 magic header. This prevents the FE
/// from accidentally bricking the database with an empty/wrong file.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn stage_restore(
    state: State<'_, AppState>,
    src_path: String,
) -> AppResult<String> {
    use crate::error::AppError;
    let src = std::path::PathBuf::from(&src_path);
    let meta = tokio::fs::metadata(&src)
        .await
        .map_err(|e| AppError::InvalidInput(format!("src_path: {e}")))?;
    if !meta.is_file() {
        return Err(AppError::InvalidInput(
            "src_path must point to a regular file".into(),
        ));
    }
    if meta.len() < 16 {
        return Err(AppError::InvalidInput(
            "src_path too small to be a SQLite database".into(),
        ));
    }
    // Verify the SQLite 3 magic header. Refuses any non-SQLite blob so a
    // typo'd path can't render the app unusable on next boot.
    let mut f = tokio::fs::File::open(&src)
        .await
        .map_err(|e| AppError::InvalidInput(format!("src_path: {e}")))?;
    let mut header = [0u8; 16];
    use tokio::io::AsyncReadExt;
    f.read_exact(&mut header)
        .await
        .map_err(|e| AppError::InvalidInput(format!("src_path header: {e}")))?;
    if &header != b"SQLite format 3\0" {
        return Err(AppError::InvalidInput(
            "src_path is not a SQLite database".into(),
        ));
    }
    drop(f);

    let pending = state.db_path.with_extension("restore_pending");
    tokio::fs::copy(&src, &pending)
        .await
        .map_err(AppError::from)?;
    Ok(pending.to_string_lossy().to_string())
}

/// Wipe all local app data and exit.
///
/// Deletes every OS keyring secret for every configured account, then writes
/// a `pending_wipe` marker file next to the database. On the next launch the
/// boot sequence detects the marker and removes the entire app data directory
/// before opening any files, giving a guaranteed clean slate.
///
/// The app exits immediately after writing the marker so no open file handles
/// block the deletion.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn clear_app_data(state: State<'_, AppState>) -> AppResult<()> {
    use crate::error::AppError;

    let accounts = state.db.list_accounts().await?;
    for account in accounts {
        let id = account.id.clone();
        match tokio::task::spawn_blocking(move || crate::secrets::delete_secret(&id)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("keyring delete failed for {}: {e}", account.id),
            Err(e) => tracing::warn!("spawn_blocking failed for {}: {e}", account.id),
        }
    }

    let app_dir = state
        .db_path
        .parent()
        .ok_or_else(|| AppError::Internal("db_path has no parent directory".into()))?;
    tokio::fs::write(app_dir.join("pending_wipe"), b"1").await?;

    // Delay exit so the IPC response reaches the frontend before the process
    // terminates. app.exit() tears down Tauri synchronously and can crash if
    // called while the command response is still in flight.
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        std::process::exit(0);
    });

    Ok(())
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn import_config(
    state: State<'_, AppState>,
    bundle: ConfigExport,
) -> AppResult<ImportSummary> {
    let mut inserted = 0usize;
    let mut updated = 0usize;
    for acct in bundle.accounts {
        let exists = state.db.get_account(&acct.id).await.is_ok();
        let a = acct.clone();
        state
            .db
            .conn
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO accounts (id, name, protocol, endpoint, region, access_key_id, addressing_style, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                     ON CONFLICT(id) DO UPDATE SET
                        name = excluded.name,
                        endpoint = excluded.endpoint,
                        region = excluded.region,
                        access_key_id = excluded.access_key_id,
                        addressing_style = excluded.addressing_style,
                        updated_at = excluded.updated_at",
                    params![
                        a.id,
                        a.name,
                        a.protocol,
                        a.endpoint,
                        a.region,
                        a.access_key_id,
                        a.addressing_style,
                        a.created_at,
                        a.updated_at,
                    ],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        if exists {
            updated += 1;
        } else {
            inserted += 1;
        }
        // Invalidate any cached client for the touched account so the next
        // call rebuilds with fresh endpoint/key.
        state.invalidate(&acct.id);
    }
    state.db.settings_save(bundle.settings).await?;
    state.invalidate_settings().await;
    Ok(ImportSummary {
        accounts_inserted: inserted,
        accounts_updated: updated,
        settings_applied: true,
    })
}
