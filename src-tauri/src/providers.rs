//! Account-to-[`ObjectStore`] factory.
//!
//! When a new provider protocol is added (e.g. Azure Blob), extend [`Protocol`]
//! with a new variant and add a branch in [`build_store`]. The rest of the
//! backend is protocol-agnostic and never touches concrete provider types.

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

/// Build an ObjectStore for the given account, pulling its secret from the keyring.
pub async fn build_store(account: &Account) -> AppResult<Arc<dyn ObjectStore>> {
    let protocol = Protocol::parse(&account.protocol)?;
    match protocol {
        Protocol::S3 => {
            // keyring::Entry::get_password is synchronous and may block on
            // D-Bus (Linux) or Keychain (macOS). Run it off the Tokio executor
            // to avoid stalling other in-flight commands.
            let account_id = account.id.clone();
            let secret = tokio::task::spawn_blocking(move || secrets::get_secret(&account_id))
                .await
                .map_err(|e| AppError::Internal(format!("keyring task panicked: {e}")))??;
            let store = S3Store::new(S3Config {
                region: account.region.clone(),
                endpoint: account.endpoint.clone(),
                access_key_id: account.access_key_id.clone(),
                secret_access_key: secret,
                addressing_style: account.addressing_style.clone(),
            })
            .await?;
            Ok(Arc::new(store))
        }
    }
}
