//! Account-to-[`ObjectStore`] factory. Adding a protocol = new [`Protocol`] variant + [`build_store`]
//! branch; the rest of the backend stays protocol-agnostic.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::db::accounts::Account;
use crate::error::{AppError, AppResult};
use crate::secrets;
use crate::store::s3::{S3Config, S3Store};
use crate::store::ObjectStore;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    S3,
}

impl Protocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            Protocol::S3 => "s3",
        }
    }

    pub fn parse(s: &str) -> AppResult<Self> {
        match s {
            "s3" => Ok(Protocol::S3),
            other => Err(AppError::InvalidInput(format!("unknown protocol: {other}"))),
        }
    }
}

async fn build_store_inner(account: &Account, region: &str, endpoint: Option<String>) -> AppResult<Arc<dyn ObjectStore>> {
    let account_id = account.id.clone();
    let secret = tokio::task::spawn_blocking(move || secrets::get_secret(&account_id))
        .await
        .map_err(|e| AppError::Internal(format!("keyring task panicked: {e}")))??;
    let store = S3Store::new(S3Config {
        region: region.to_string(),
        endpoint,
        access_key_id: account.access_key_id.clone(),
        secret_access_key: secret,
        addressing_style: account.addressing_style.clone(),
    })
    .await?;
    Ok(Arc::new(store))
}

/// Minimal us-east-1/global-endpoint store for cross-region probes (GetBucketLocation), so region
/// auto-correction never probes through a misconfigured-region client.
pub async fn build_probe_store(account: &Account) -> AppResult<Arc<dyn ObjectStore>> {
    build_store_inner(account, "us-east-1", None).await
}

/// Like [`build_store`] but signs for an explicit region (per-bucket region routing).
pub async fn build_store_with_region(
    account: &Account,
    region: &str,
) -> AppResult<Arc<dyn ObjectStore>> {
    build_store_inner(account, region, account.endpoint.clone()).await
}

pub async fn build_store(account: &Account) -> AppResult<Arc<dyn ObjectStore>> {
    Protocol::parse(&account.protocol)?;
    build_store_inner(account, &account.region, account.endpoint.clone()).await
}
