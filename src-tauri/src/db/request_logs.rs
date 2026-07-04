use chrono::Utc;
use rusqlite::params;
use rusqlite::types::Value;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::AppResult;

use super::Db;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLog {
    pub id: String,
    pub account_id: Option<String>,
    pub account_name: Option<String>,
    pub operation: String,
    pub http_method: Option<String>,
    pub request_url: Option<String>,
    pub request_params: Option<String>,
    pub response_meta: Option<String>,
    pub bucket: Option<String>,
    pub key: Option<String>,
    pub status: String,
    pub response_status: Option<i64>,
    pub error_code: Option<String>,
    pub error_msg: Option<String>,
    pub duration_ms: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone, Default)]
pub struct NewRequestLog {
    pub account_id: Option<String>,
    pub account_name: Option<String>,
    pub operation: String,
    pub http_method: Option<String>,
    pub request_url: Option<String>,
    pub request_params: Option<String>,
    pub response_meta: Option<String>,
    pub bucket: Option<String>,
    pub key: Option<String>,
    pub status: String,
    pub response_status: Option<i64>,
    pub error_code: Option<String>,
    pub error_msg: Option<String>,
    pub duration_ms: i64,
}

#[derive(Debug, Clone, Default)]
pub struct RequestLogFilter {
    pub search: Option<String>,
    /// Exact match on the `status` column ("ok" / "error").
    pub status: Option<String>,
    /// Exact match on the `operation` column.
    pub operation: Option<String>,
}

impl RequestLogFilter {
    fn to_sql(&self) -> (String, Vec<Value>) {
        let mut clauses: Vec<String> = Vec::new();
        let mut values: Vec<Value> = Vec::new();
        if let Some(q) = self.search.as_ref().filter(|s| !s.trim().is_empty()) {
            let escaped = q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
            let pattern = format!("%{}%", escaped);
            let n = values.len() + 1;
            clauses.push(format!(
                "(operation LIKE ?{n} ESCAPE '\\' OR bucket LIKE ?{n} ESCAPE '\\' \
                  OR key LIKE ?{n} ESCAPE '\\' OR account_name LIKE ?{n} ESCAPE '\\' \
                  OR error_msg LIKE ?{n} ESCAPE '\\' OR request_url LIKE ?{n} ESCAPE '\\' \
                  OR request_params LIKE ?{n} ESCAPE '\\')"
            ));
            values.push(Value::Text(pattern));
        }
        if let Some(s) = self.status.as_ref().filter(|s| !s.trim().is_empty()) {
            clauses.push(format!("status = ?{}", values.len() + 1));
            values.push(Value::Text(s.clone()));
        }
        if let Some(op) = self.operation.as_ref().filter(|s| !s.trim().is_empty()) {
            clauses.push(format!("operation = ?{}", values.len() + 1));
            values.push(Value::Text(op.clone()));
        }
        let where_sql = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        (where_sql, values)
    }
}

impl Db {
    pub async fn insert_request_log(&self, log: NewRequestLog) -> AppResult<()> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().timestamp();
        self.conn
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO request_logs \
                     (id, account_id, account_name, operation, http_method, request_url, \
                      request_params, response_meta, bucket, key, status, response_status, \
                      error_code, error_msg, duration_ms, created_at) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                    params![
                        id,
                        log.account_id,
                        log.account_name,
                        log.operation,
                        log.http_method,
                        log.request_url,
                        log.request_params,
                        log.response_meta,
                        log.bucket,
                        log.key,
                        log.status,
                        log.response_status,
                        log.error_code,
                        log.error_msg,
                        log.duration_ms,
                        now,
                    ],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }

    pub async fn list_request_logs(
        &self,
        limit: u32,
        offset: u32,
        filter: RequestLogFilter,
    ) -> AppResult<Vec<RequestLog>> {
        let rows = self
            .conn
            .call(move |conn| {
                let cols = "id, account_id, account_name, operation, http_method, \
                            request_url, request_params, response_meta, bucket, key, status, \
                            response_status, error_code, error_msg, duration_ms, created_at";
                let (where_sql, mut values) = filter.to_sql();
                // rowid DESC: newest first, correct ordering within the same second.
                let sql = format!(
                    "SELECT {cols} FROM request_logs {where_sql} \
                     ORDER BY rowid DESC LIMIT ? OFFSET ?"
                );
                values.push(Value::Integer(limit as i64));
                values.push(Value::Integer(offset as i64));
                let mut stmt = conn.prepare(&sql)?;
                let v: Vec<RequestLog> = stmt
                    .query_map(rusqlite::params_from_iter(values), map_row)?
                    .filter_map(|r| r.map_err(|e| tracing::warn!("request_log map_row failed: {e}")).ok())
                    .collect();
                Ok::<_, tokio_rusqlite::Error>(v)
            })
            .await?;
        Ok(rows)
    }

    pub async fn count_request_logs(&self, filter: RequestLogFilter) -> AppResult<i64> {
        let n: i64 = self
            .conn
            .call(move |conn| {
                let (where_sql, values) = filter.to_sql();
                let n: i64 = conn.query_row(
                    &format!("SELECT COUNT(*) FROM request_logs {where_sql}"),
                    rusqlite::params_from_iter(values),
                    |r| r.get(0),
                )?;
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?;
        Ok(n)
    }

    pub async fn delete_old_request_logs(&self, before_ts: i64) -> AppResult<u64> {
        let n = self
            .conn
            .call(move |conn| {
                let n = conn.execute(
                    "DELETE FROM request_logs WHERE created_at < ?1",
                    params![before_ts],
                )?;
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?;
        Ok(n as u64)
    }

    pub async fn clear_all_request_logs(&self) -> AppResult<()> {
        self.conn
            .call(|conn| {
                conn.execute("DELETE FROM request_logs", [])?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(())
    }
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RequestLog> {
    Ok(RequestLog {
        id: row.get(0)?,
        account_id: row.get(1)?,
        account_name: row.get(2)?,
        operation: row.get(3)?,
        http_method: row.get(4)?,
        request_url: row.get(5)?,
        request_params: row.get(6)?,
        response_meta: row.get(7)?,
        bucket: row.get(8)?,
        key: row.get(9)?,
        status: row.get(10)?,
        response_status: row.get(11)?,
        error_code: row.get(12)?,
        error_msg: row.get(13)?,
        duration_ms: row.get(14)?,
        created_at: row.get(15)?,
    })
}
