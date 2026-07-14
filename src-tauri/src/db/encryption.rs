use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use crate::error::AppResult;

use super::Db;

#[derive(Debug, Clone, Serialize)]
pub struct BucketEncryptionConfig {
    pub account_id: String,
    pub bucket: String,
    /// bech32 age recipient (`age1...`). Public: safe to persist and echo to
    /// the FE. The corresponding secret identity lives in the OS keychain.
    /// Column name `salt_hex` is legacy from the pre-age implementation.
    pub recipient: String,
}

impl Db {
    pub async fn get_encryption_config(
        &self,
        account_id: &str,
        bucket: &str,
    ) -> AppResult<Option<BucketEncryptionConfig>> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        self.conn
            .call(move |conn| {
                conn.query_row(
                    "SELECT account_id, bucket, salt_hex FROM bucket_encryption \
                     WHERE account_id=?1 AND bucket=?2",
                    params![account_id, bucket],
                    |row| {
                        Ok(BucketEncryptionConfig {
                            account_id: row.get(0)?,
                            bucket: row.get(1)?,
                            recipient: row.get(2)?,
                        })
                    },
                )
                .optional()
                .map_err(tokio_rusqlite::Error::from)
            })
            .await
            .map_err(Into::into)
    }

    pub async fn set_encryption_config(
        &self,
        account_id: &str,
        bucket: &str,
        recipient: &str,
    ) -> AppResult<()> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        let salt_hex = recipient.to_string();
        let now = chrono::Utc::now().timestamp();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO bucket_encryption(account_id, bucket, salt_hex, created_at) \
                     VALUES(?1,?2,?3,?4) \
                     ON CONFLICT(account_id, bucket) DO UPDATE SET salt_hex=excluded.salt_hex",
                    params![account_id, bucket, salt_hex, now],
                )
                .map(|_| ())
                .map_err(tokio_rusqlite::Error::from)
            })
            .await
            .map_err(Into::into)
    }

    pub async fn delete_encryption_config(&self, account_id: &str, bucket: &str) -> AppResult<()> {
        let account_id = account_id.to_string();
        let bucket = bucket.to_string();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM bucket_encryption WHERE account_id=?1 AND bucket=?2",
                    params![account_id, bucket],
                )
                .map(|_| ())
                .map_err(tokio_rusqlite::Error::from)
            })
            .await
            .map_err(Into::into)
    }
}
