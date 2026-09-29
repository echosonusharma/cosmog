use serde::Deserialize;
use tauri::State;

use crate::db::accounts::{Account, NewAccount, UpdateAccount};
use crate::error::{AppError, AppResult};
use crate::providers::Protocol;
use crate::secrets;
use crate::state::AppState;
use crate::validate;

/// Applies non-empty + length-cap validation to an optional field, preserving absence.
fn validated_optional(field: &str, value: &Option<String>) -> AppResult<Option<String>> {
    value.as_deref().map(|v| validate::require_non_empty(field, v)).transpose()
}

fn validated_endpoint(value: &Option<String>) -> AppResult<Option<String>> {
    value.as_deref().map(validate::validate_endpoint).transpose()
}

fn validated_addressing(value: Option<String>) -> AppResult<Option<String>> {
    match value.as_deref() {
        None | Some("auto" | "path" | "virtual") => Ok(value),
        Some(other) => Err(AppError::InvalidInput(format!("unknown addressing_style: {other}"))),
    }
}

/// Ids double as keyring keys, so only UUIDs (all the app ever generated) pass.
pub(crate) fn sanitize_imported(acct: Account) -> AppResult<Account> {
    Protocol::parse(&acct.protocol)?;
    let id = uuid::Uuid::parse_str(&acct.id)
        .map_err(|_| AppError::InvalidInput(format!("account id is not a UUID: {}", acct.id)))?
        .to_string();
    // Pre-validation versions stored bare hosts; the SDK needs a scheme.
    let endpoint = acct.endpoint.and_then(|e| match e.trim() {
        "" => None,
        t if !t.contains("://") => Some(format!("https://{t}")),
        t => Some(t.to_string()),
    });
    let region = match acct.region.trim() {
        "" => "us-east-1".to_string(),
        _ => validate::require_non_empty("region", &acct.region)?,
    };
    Ok(Account {
        id,
        name: validate::require_non_empty("name", &acct.name)?,
        protocol: acct.protocol,
        endpoint: validated_endpoint(&endpoint)?,
        region,
        access_key_id: validate::require_non_empty("access_key_id", &acct.access_key_id)?,
        addressing_style: validated_addressing(Some(acct.addressing_style))?
            .unwrap_or_else(|| "auto".into()),
        created_at: acct.created_at,
        updated_at: acct.updated_at,
    })
}

pub(crate) async fn delete_account_secrets(id: String, enc_buckets: Vec<String>) {
    let res = tokio::task::spawn_blocking(move || {
        if let Err(e) = secrets::delete_secret(&id) {
            tracing::warn!(account_id = %id, "delete_secret failed: {e}; keyring entry may be orphaned");
        }
        for bucket in enc_buckets {
            if let Err(e) = secrets::delete_enc_identity(&id, &bucket) {
                tracing::warn!(account_id = %id, bucket, "delete_enc_identity failed: {e}");
            }
        }
    })
    .await;
    if let Err(e) = res {
        tracing::warn!("keyring cleanup task failed: {e}");
    }
}

/// Runs the synchronous keyring write off the async runtime; on macOS a
/// prompt or locked keychain can stall for seconds.
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
    /// Optional — defaults to `"us-east-1"`; for AWS, auto-detected on first
    /// access via PermanentRedirect recovery.
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
    let endpoint = validated_endpoint(&input.endpoint)?;
    let region = validated_optional("region", &input.region)?;
    let access_key_id = validate::require_non_empty("access_key_id", &input.access_key_id)?;
    let addressing_style = validated_addressing(input.addressing_style)?;
    let acct = state
        .db
        .insert_account(NewAccount {
            name,
            protocol: input.protocol,
            endpoint,
            region: region.unwrap_or_else(|| "us-east-1".to_string()),
            access_key_id,
            addressing_style,
        })
        .await?;
    // Secret written AFTER the DB insert (the id is needed); on keyring
    // failure roll back the row so no orphan is left.
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
    #[serde(default, deserialize_with = "crate::validate::double_option")]
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
        // inner Some(String) is validated.
        Some(inner) => Some(validated_endpoint(&inner)?),
        None => None,
    };
    let region = validated_optional("region", &input.region)?;
    let access_key_id = validated_optional("access_key_id", &input.access_key_id)?;
    let addressing_style = validated_addressing(input.addressing_style)?;
    state.db.get_account(&id).await?;
    // Secret first so a keyring failure leaves the row untouched.
    if let Some(secret) = input.secret_access_key {
        let res = set_secret_blocking(id.clone(), secret).await;
        state.invalidate(&id);
        res?;
    }
    let res = state
        .db
        .update_account(
            &id,
            UpdateAccount {
                name,
                endpoint,
                region,
                access_key_id,
                addressing_style,
            },
        )
        .await;
    state.invalidate(&id);
    res
}

#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn delete_account(state: State<'_, AppState>, id: String) -> AppResult<()> {
    // Cancel active transfers first so workers stop before the DB rows are
    // cascade-deleted (transfers/cache/index rows swept by ON DELETE CASCADE).
    if let Err(e) = state.transfers.cancel_for_account(&id).await {
        tracing::warn!(account_id = %id, "cancel_for_account failed: {e}");
    }
    state.cancel_all_scans_for_account(&id);
    // Read encrypted buckets before the cascade drops their rows.
    let enc_buckets = state
        .db
        .list_encrypted_buckets_for_account(&id)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(account_id = %id, "list encrypted buckets failed: {e}");
            Vec::new()
        });
    state.db.delete_account(&id).await?;
    delete_account_secrets(id.clone(), enc_buckets).await;
    state.invalidate(&id);
    Ok(())
}

/// Lightweight connectivity check — ensures credentials work by listing buckets.
#[tracing::instrument(skip_all, err)]
#[tauri::command]
pub async fn test_account(state: State<'_, AppState>, id: String) -> AppResult<usize> {
    // Invalidate so each probe builds a fresh connection (doubles as reconnect).
    state.invalidate(&id);
    let store = state.store_for(&id).await?;
    let buckets = store.list_buckets().await?;
    Ok(buckets.len())
}

#[derive(Debug, serde::Serialize)]
pub struct RegionDetectResult {
    /// Region as reported by the bucket; `None` = us-east-1 convention
    /// (empty `LocationConstraint`).
    pub region: Option<String>,
    /// True if we updated the stored account region to match.
    pub updated: bool,
}

/// Detects a bucket's real region and persists it on the account when it
/// differs (fixes PermanentRedirect / SignatureDoesNotMatch).
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

#[cfg(test)]
mod tests {
    use super::*;

    fn acct(id: &str, endpoint: Option<&str>) -> Account {
        Account {
            id: id.into(),
            name: "n".into(),
            protocol: "s3".into(),
            endpoint: endpoint.map(Into::into),
            region: "".into(),
            access_key_id: "AK".into(),
            addressing_style: "auto".into(),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn import_rejects_non_uuid_ids() {
        assert!(sanitize_imported(acct("mcp_bearer_token", None)).is_err());
        let id = uuid::Uuid::new_v4().to_string();
        assert_eq!(sanitize_imported(acct(&id, None)).unwrap().id, id);
    }

    #[test]
    fn import_repairs_legacy_endpoints() {
        let id = uuid::Uuid::new_v4().to_string();
        let a = sanitize_imported(acct(&id, Some("minio.local:9000"))).unwrap();
        assert_eq!(a.endpoint.as_deref(), Some("https://minio.local:9000"));
        assert_eq!(a.region, "us-east-1");
        assert_eq!(sanitize_imported(acct(&id, Some(" "))).unwrap().endpoint, None);
        let v6 = sanitize_imported(acct(&id, Some("http://[::1]:9000/p"))).unwrap();
        assert_eq!(v6.endpoint.as_deref(), Some("http://[::1]:9000/p"));
        assert!(sanitize_imported(acct(&id, Some("ftp://x"))).is_err());
    }
}
