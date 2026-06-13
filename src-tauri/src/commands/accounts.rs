use serde::Deserialize;
use tauri::State;

use crate::db::accounts::{Account, NewAccount, UpdateAccount};
use crate::error::AppResult;
use crate::providers::Protocol;
use crate::secrets;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct AddAccountInput {
    pub name: String,
    pub protocol: String,
    pub endpoint: Option<String>,
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub addressing_style: Option<String>,
}

// Custom Debug to keep `secret_access_key` out of log lines if anything ever
// tries to format the struct. `tracing::instrument(skip_all)` already drops
// the arg today, but this is belt-and-braces.
impl std::fmt::Debug for AddAccountInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddAccountInput")
            .field("name", &self.name)
            .field("protocol", &self.protocol)
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"<redacted>")
            .field("addressing_style", &self.addressing_style)
            .finish()
    }
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn add_account(
    state: State<'_, AppState>,
    input: AddAccountInput,
) -> AppResult<Account> {
    Protocol::parse(&input.protocol)?;
    let acct = state
        .db
        .insert_account(NewAccount {
            name: input.name,
            protocol: input.protocol,
            endpoint: input.endpoint,
            region: input.region,
            access_key_id: input.access_key_id,
            addressing_style: input.addressing_style,
        })
        .await?;
    secrets::set_secret(&acct.id, &input.secret_access_key)?;
    Ok(acct)
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn list_accounts(state: State<'_, AppState>) -> AppResult<Vec<Account>> {
    state.db.list_accounts().await
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn get_account(state: State<'_, AppState>, id: String) -> AppResult<Account> {
    state.db.get_account(&id).await
}

#[derive(Deserialize)]
pub struct UpdateAccountInput {
    pub name: Option<String>,
    pub endpoint: Option<Option<String>>,
    pub region: Option<String>,
    pub access_key_id: Option<String>,
    pub addressing_style: Option<String>,
    /// If supplied, the secret is rotated in the keyring.
    pub secret_access_key: Option<String>,
}

impl std::fmt::Debug for UpdateAccountInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateAccountInput")
            .field("name", &self.name)
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("access_key_id", &self.access_key_id)
            .field("addressing_style", &self.addressing_style)
            .field("secret_access_key", &self.secret_access_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn update_account(
    state: State<'_, AppState>,
    id: String,
    input: UpdateAccountInput,
) -> AppResult<Account> {
    let acct = state
        .db
        .update_account(
            &id,
            UpdateAccount {
                name: input.name,
                endpoint: input.endpoint,
                region: input.region,
                access_key_id: input.access_key_id,
                addressing_style: input.addressing_style,
            },
        )
        .await?;
    if let Some(secret) = input.secret_access_key {
        secrets::set_secret(&id, &secret)?;
    }
    state.invalidate(&id);
    Ok(acct)
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn delete_account(state: State<'_, AppState>, id: String) -> AppResult<()> {
    // Signal every active transfer for this account so the workers stop
    // before the DB rows get cascade-deleted. The ON DELETE CASCADE will
    // then sweep up transfers/cached_objects/bucket_index/prefix_sync rows.
    let _ = state.transfers.cancel_for_account(&id).await;
    state.cancel_all_scans_for_account(&id);
    state.db.delete_account(&id).await?;
    if let Err(e) = secrets::delete_secret(&id) {
        tracing::warn!(account_id = %id, "delete_secret failed: {e}; keyring entry may be orphaned");
    }
    state.invalidate(&id);
    Ok(())
}

/// Lightweight connectivity check — ensures credentials work by listing buckets.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn test_account(state: State<'_, AppState>, id: String) -> AppResult<usize> {
    let store = state.store_for(&id).await?;
    let buckets = store.list_buckets().await?;
    Ok(buckets.len())
}

#[derive(Debug, serde::Serialize)]
pub struct RegionDetectResult {
    /// Region as reported by the bucket. `None` for `us-east-1` per S3
    /// protocol convention (an empty `LocationConstraint`).
    pub region: Option<String>,
    /// `true` if we updated the stored account region to match.
    pub updated: bool,
}

/// Detect a bucket's real region and persist it on the account if it differs
/// from the configured value. Useful when the user creates an account with
/// the wrong region and gets PermanentRedirect / SignatureDoesNotMatch.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn detect_account_region(
    state: State<'_, AppState>,
    account_id: String,
    bucket: String,
) -> AppResult<RegionDetectResult> {
    let store = state.store_for(&account_id).await?;
    let region = store.get_bucket_location(&bucket).await?;
    let acct = state.db.get_account(&account_id).await?;
    let target = region.clone().unwrap_or_else(|| "us-east-1".to_string());
    let updated = if acct.region != target {
        state
            .db
            .update_account(
                &account_id,
                crate::db::accounts::UpdateAccount {
                    name: None,
                    endpoint: None,
                    region: Some(target),
                    access_key_id: None,
                    addressing_style: None,
                },
            )
            .await?;
        state.invalidate(&account_id);
        true
    } else {
        false
    };
    Ok(RegionDetectResult { region, updated })
}
