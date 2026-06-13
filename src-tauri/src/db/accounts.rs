use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AppError, AppResult};

use super::Db;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub name: String,
    pub protocol: String,
    pub endpoint: Option<String>,
    pub region: String,
    pub access_key_id: String,
    pub addressing_style: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewAccount {
    pub name: String,
    pub protocol: String,
    pub endpoint: Option<String>,
    pub region: String,
    pub access_key_id: String,
    pub addressing_style: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateAccount {
    pub name: Option<String>,
    pub endpoint: Option<Option<String>>,
    pub region: Option<String>,
    pub access_key_id: Option<String>,
    pub addressing_style: Option<String>,
}

fn map_row(row: &rusqlite::Row) -> rusqlite::Result<Account> {
    Ok(Account {
        id: row.get(0)?,
        name: row.get(1)?,
        protocol: row.get(2)?,
        endpoint: row.get(3)?,
        region: row.get(4)?,
        access_key_id: row.get(5)?,
        addressing_style: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

impl Db {
    pub async fn insert_account(&self, new: NewAccount) -> AppResult<Account> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().timestamp();
        let style = new.addressing_style.unwrap_or_else(|| "auto".into());
        let acct = Account {
            id: id.clone(),
            name: new.name,
            protocol: new.protocol,
            endpoint: new.endpoint,
            region: new.region,
            access_key_id: new.access_key_id,
            addressing_style: style,
            created_at: now,
            updated_at: now,
        };
        let to_insert = acct.clone();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO accounts (id, name, protocol, endpoint, region, access_key_id, addressing_style, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        to_insert.id,
                        to_insert.name,
                        to_insert.protocol,
                        to_insert.endpoint,
                        to_insert.region,
                        to_insert.access_key_id,
                        to_insert.addressing_style,
                        to_insert.created_at,
                        to_insert.updated_at,
                    ],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(acct)
    }

    pub async fn list_accounts(&self) -> AppResult<Vec<Account>> {
        let rows = self
            .conn
            .call(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT id, name, protocol, endpoint, region, access_key_id, addressing_style, created_at, updated_at FROM accounts ORDER BY created_at DESC",
                )?;
                let iter = stmt.query_map([], map_row)?;
                let mut out = Vec::new();
                for row in iter {
                    out.push(row?);
                }
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await?;
        Ok(rows)
    }

    pub async fn get_account(&self, id: &str) -> AppResult<Account> {
        let id = id.to_string();
        let row = self
            .conn
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT id, name, protocol, endpoint, region, access_key_id, addressing_style, created_at, updated_at FROM accounts WHERE id = ?1",
                )?;
                let row = stmt.query_row(params![id], map_row).optional()?;
                Ok::<_, tokio_rusqlite::Error>(row)
            })
            .await?;
        row.ok_or_else(|| AppError::NotFound(format!("account")))
    }

    pub async fn update_account(&self, id: &str, patch: UpdateAccount) -> AppResult<Account> {
        let existing = self.get_account(id).await?;
        let merged = Account {
            id: existing.id.clone(),
            name: patch.name.unwrap_or(existing.name),
            protocol: existing.protocol,
            endpoint: match patch.endpoint {
                Some(v) => v,
                None => existing.endpoint,
            },
            region: patch.region.unwrap_or(existing.region),
            access_key_id: patch.access_key_id.unwrap_or(existing.access_key_id),
            addressing_style: patch.addressing_style.unwrap_or(existing.addressing_style),
            created_at: existing.created_at,
            updated_at: Utc::now().timestamp(),
        };
        let to_update = merged.clone();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "UPDATE accounts SET name=?1, endpoint=?2, region=?3, access_key_id=?4, addressing_style=?5, updated_at=?6 WHERE id=?7",
                    params![
                        to_update.name,
                        to_update.endpoint,
                        to_update.region,
                        to_update.access_key_id,
                        to_update.addressing_style,
                        to_update.updated_at,
                        to_update.id,
                    ],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(merged)
    }

    pub async fn delete_account(&self, id: &str) -> AppResult<()> {
        let id = id.to_string();
        let affected = self
            .conn
            .call(move |conn| {
                let n = conn.execute("DELETE FROM accounts WHERE id = ?1", params![id])?;
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?;
        if affected == 0 {
            return Err(AppError::NotFound("account".into()));
        }
        Ok(())
    }
}
