//! Backup/restore commands. Exports carry accounts (sans secrets — keyring is
//! never serialized) + settings; import merges by account id instead of replacing.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::db::accounts::Account;
use crate::db::settings::AppSettings;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// Versioned snapshot of a Cosmog install, so future schema changes can be
/// migrated on import.
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
    pub settings_skipped: Vec<String>,
}

/// Copies the live SQLite file to `dest_path` after a WAL checkpoint; does NOT
/// include OS-keyring secrets.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn backup_database(
    state: State<'_, AppState>,
    dest_path: String,
) -> AppResult<()> {
    let dest = std::path::PathBuf::from(dest_path);
    state.db.backup_to(dest).await
}

/// Validates the source is a SQLite DB, then stages it as
/// `<db_path>.restore_pending` for atomic application at next boot.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn stage_restore(
    state: State<'_, AppState>,
    src_path: String,
) -> AppResult<String> {
    static STAGE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = STAGE_LOCK.lock().await;

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

    let dir = state
        .db_path
        .parent()
        .ok_or_else(|| AppError::Internal("db_path has no parent directory".into()))?
        .to_path_buf();
    let pending = state.db_path.with_extension("restore_pending");
    // Unique temp name in the SAME directory as the target so the final
    // rename is atomic; a crash can't leave a half-written pending file.
    let staging = dir.join(format!(".restore_stage.{}", uuid::Uuid::new_v4()));
    let copied = tokio::fs::copy(&src, &staging).await;
    if let Err(e) = copied {
        let _ = tokio::fs::remove_file(&staging).await;
        return Err(AppError::from(e));
    }
    if let Err(e) = tokio::fs::rename(&staging, &pending).await {
        let _ = tokio::fs::remove_file(&staging).await;
        return Err(AppError::from(e));
    }
    Ok(pending.to_string_lossy().to_string())
}

/// Wipes local data and exits: deletes every keyring secret, writes a
/// `pending_wipe` marker the next boot consumes to remove the app data dir.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn clear_app_data(state: State<'_, AppState>) -> AppResult<()> {
    #[cfg(not(target_os = "android"))]
    crate::mcp::stop().await;
    let accounts = state.db.list_accounts().await?;
    for account in accounts {
        let enc_buckets = state
            .db
            .list_encrypted_buckets_for_account(&account.id)
            .await
            .unwrap_or_default();
        super::accounts::delete_account_secrets(account.id, enc_buckets).await;
    }
    #[cfg(not(target_os = "android"))]
    if let Err(e) = crate::mcp::delete_token().await {
        tracing::warn!("MCP token delete failed: {e}");
    }

    let app_dir = state
        .db_path
        .parent()
        .ok_or_else(|| AppError::Internal("db_path has no parent directory".into()))?;
    tokio::fs::write(app_dir.join("pending_wipe"), b"1").await?;

    // Delay exit so the IPC response lands first; app.exit() mid-response
    // tears down Tauri synchronously and can crash.
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
    // Refuse bundles from a NEWER build: serde would silently drop their extra
    // fields, corrupting the config; older versions default fine.
    if bundle.schema_version > EXPORT_SCHEMA {
        return Err(AppError::InvalidInput(format!(
            "unsupported export schema_version {} (this build supports up to {EXPORT_SCHEMA}); \
             update Cosmog on this machine and retry the import",
            bundle.schema_version
        )));
    }
    let accounts = bundle
        .accounts
        .into_iter()
        .map(super::accounts::sanitize_imported)
        .collect::<AppResult<Vec<_>>>()?;
    let mut inserted = 0usize;
    let mut updated = 0usize;
    for acct in accounts {
        let exists = state.db.get_account(&acct.id).await.is_ok();
        state.db.upsert_account(acct.clone()).await?;
        if exists {
            updated += 1;
        } else {
            inserted += 1;
        }
        state.invalidate(&acct.id);
    }
    let current = state.load_settings().await?;
    let (settings, skipped) = merge_imported_settings(bundle.settings, &current);
    let saved = state.db.settings_save(settings).await?;
    state.invalidate_settings().await;
    state.set_transfer_concurrency(saved.transfer_concurrency as usize);
    // Data is already imported; a bind failure must not report the import as failed.
    #[cfg(not(target_os = "android"))]
    if let Err(e) = crate::mcp::apply(&state).await {
        tracing::warn!("MCP re-apply after import failed: {e}");
    }
    Ok(ImportSummary {
        accounts_inserted: inserted,
        accounts_updated: updated,
        settings_applied: true,
        settings_skipped: skipped,
    })
}

/// A bundle must not grant MCP access or reroute traffic, so those keep local values.
fn merge_imported_settings(
    mut incoming: AppSettings,
    current: &AppSettings,
) -> (AppSettings, Vec<String>) {
    let mut skipped = Vec::new();
    macro_rules! keep_local {
        ($($field:ident),*) => {$(
            if incoming.$field != current.$field {
                skipped.push(stringify!($field).to_string());
            }
            incoming.$field = current.$field.clone();
        )*};
    }
    keep_local!(
        mcp_enabled,
        mcp_allow_write,
        mcp_allow_delete,
        mcp_bind_all_accounts,
        mcp_acknowledged,
        mcp_fs_root,
        mcp_disabled_accounts,
        mcp_disabled_tools,
        http_proxy,
        custom_ca_path
    );
    (incoming, skipped)
}
