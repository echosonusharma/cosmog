use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use crate::error::AppResult;

use super::Db;

#[derive(Debug, Clone, Serialize)]
pub struct BucketEncryptionConfig {
    pub account_id: String,
    pub bucket: String,
    /// Public bech32 age recipient (`age1...`); the secret identity lives in
    /// the OS keychain. Column `salt_hex` is legacy pre-age naming.
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

    /// Bucket names with an encryption config, for FE lock badges in one round-trip.
    pub async fn list_encrypted_buckets_for_account(
        &self,
        account_id: &str,
    ) -> AppResult<Vec<String>> {
        let account_id = account_id.to_string();
        self.conn
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT bucket FROM bucket_encryption WHERE account_id=?1",
                )?;
                let rows = stmt.query_map(params![account_id], |row| row.get::<_, String>(0))?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r?);
                }
                Ok::<_, tokio_rusqlite::Error>(out)
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
