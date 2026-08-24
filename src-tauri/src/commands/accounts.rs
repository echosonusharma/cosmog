use serde::Deserialize;
use tauri::State;

use crate::db::accounts::{Account, NewAccount, UpdateAccount};
use crate::error::{AppError, AppResult};
use crate::providers::Protocol;
use crate::secrets;
use crate::state::AppState;
use crate::validate;

/// Apply `require_non_empty`'s non-empty + length-cap rules to an optional
/// field, preserving absence (`None` stays `None`).
fn validated_optional(field: &str, value: &Option<String>) -> AppResult<Option<String>> {
    value.as_deref().map(|v| validate::require_non_empty(field, v)).transpose()
}

/// Run the synchronous keyring write off the async runtime. `keyring` blocks;
/// on macOS a prompt or locked keychain can stall for seconds.
async fn set_secret_blocking(id: String, secret: String) -> AppResult<()> {
    tokio::task::spawn_blocking(move || secrets::set_secret(&id, &secret))
        .await
        .map_err(|e| AppError::Internal(format!("keyring task failed: {e}")))?
}

#[derive(Deserialize)]
pub struct AddAccountInput {
    pub name: String,
    pub protocol: String,
    pub endpoint: Option<String>,
    /// Optional — defaults to `"us-east-1"`. For AWS S3 accounts the real
    /// region is auto-detected on first access via PermanentRedirect recovery.
    pub region: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub addressing_style: Option<String>,
}

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
    let name = validate::require_non_empty("name", &input.name)?;
    let endpoint = validated_optional("endpoint", &input.endpoint)?;
    let region = validated_optional("region", &input.region)?;
    let access_key_id = validate::require_non_empty("access_key_id", &input.access_key_id)?;
    let acct = state
        .db
        .insert_account(NewAccount {
            name,
            protocol: input.protocol,
            endpoint,
            region: region.unwrap_or_else(|| "us-east-1".to_string()),
            access_key_id,
            addressing_style: input.addressing_style,
        })
        .await?;
    // Write secret AFTER DB insert (we need the generated id).
    // On keyring failure roll back the DB row so no orphan is left.
    // The keyring write runs on a blocking thread (see set_secret_blocking).
    if let Err(e) =
        set_secret_blocking(acct.id.clone(), input.secret_access_key).await
    {
        if let Err(del_err) = state.db.delete_account(&acct.id).await {
            tracing::error!(account_id = %acct.id, "failed to roll back account after keyring write failed: {del_err}");
        }
        return Err(e);
    }
    Ok(acct)
}

/// Account row + whether its secret is still in the keychain.
#[derive(Debug, serde::Serialize)]
pub struct AccountView {
    #[serde(flatten)]
    pub account: Account,
    pub needs_reauth: bool,
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn list_accounts(state: State<'_, AppState>) -> AppResult<Vec<AccountView>> {
    let accounts = state.db.list_accounts().await?;
    Ok(accounts
        .into_iter()
        .map(|account| {
            // unwrap_or(true): transient probe failure = assume present, don't flag.
            let needs_reauth = !secrets::secret_present(&account.id).unwrap_or(true);
            AccountView { account, needs_reauth }
        })
        .collect())
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
    let name = validated_optional("name", &input.name)?;
    let endpoint = match input.endpoint {
        // Double-Option: Some(None) explicitly clears the endpoint; only the
        // inner Some(String) is subject to the non-empty/length rules.
        Some(inner) => Some(validated_optional("endpoint", &inner)?),
        None => None,
    };
    let region = validated_optional("region", &input.region)?;
    let access_key_id = validated_optional("access_key_id", &input.access_key_id)?;
    let acct = state
        .db
        .update_account(
            &id,
            UpdateAccount {
                name,
                endpoint,
                region,
                access_key_id,
                addressing_style: input.addressing_style,
            },
        )
        .await?;
    if let Some(secret) = input.secret_access_key {
        set_secret_blocking(id, secret).await?;
    }
    state.invalidate(&acct.id);
    Ok(acct)
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn delete_account(state: State<'_, AppState>, id: String) -> AppResult<()> {
    // Signal every active transfer for this account so the workers stop
    // before the DB rows get cascade-deleted. The ON DELETE CASCADE will
    // then sweep up transfers/cached_objects/bucket_index/prefix_sync rows.
    if let Err(e) = state.transfers.cancel_for_account(&id).await {
        tracing::warn!(account_id = %id, "cancel_for_account failed: {e}");
    }
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
    // Invalidate cached client so every test probe builds a fresh connection.
    // This doubles as a reconnect when the server was restarted.
    state.invalidate(&id);
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
