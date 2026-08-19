use chrono::Utc;
use rusqlite::params;
use rusqlite::types::Value;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::error::AppResult;

use super::Db;

/// Hard cap on retained request-log rows, enforced on every prune regardless of
/// TTL. Keeps the table (and its full-table search scans) bounded.
pub const REQUEST_LOG_MAX_ROWS: i64 = 50_000;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLogAccountStat {
    pub account_id: Option<String>,
    pub account_name: Option<String>,
    pub count: i64,
    pub error_count: i64,
    pub avg_duration_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLogOperationStat {
    pub operation: String,
    pub count: i64,
    pub error_count: i64,
    pub avg_duration_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLogDayStat {
    pub day: i64,
    pub count: i64,
    pub error_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLogDayAccountStat {
    pub day: i64,
    pub account_id: Option<String>,
    pub account_name: Option<String>,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLogBucketStat {
    pub bucket: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLogStats {
    pub period_days: u32,
    pub since_ts: i64,
    pub total: i64,
    pub ok_count: i64,
    pub error_count: i64,
    pub avg_duration_ms: i64,
    pub by_account: Vec<RequestLogAccountStat>,
    pub by_operation: Vec<RequestLogOperationStat>,
    pub by_day: Vec<RequestLogDayStat>,
    pub by_day_by_account: Vec<RequestLogDayAccountStat>,
    pub top_buckets: Vec<RequestLogBucketStat>,
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

    /// Prune request logs by age (TTL) AND a hard row cap. The row cap bounds
    /// the fastest-growing table between TTL passes: a bulk operation can emit
    /// thousands of rows in minutes, and leading-wildcard log searches scan the
    /// whole table. Both deletes run in one transaction.
    pub async fn delete_old_request_logs(&self, before_ts: i64) -> AppResult<u64> {
        let n = self
            .conn
            .call(move |conn| {
                let tx = conn.transaction()?;
                let mut n = tx.execute(
                    "DELETE FROM request_logs WHERE created_at < ?1",
                    params![before_ts],
                )?;
                n += tx.execute(
                    "DELETE FROM request_logs WHERE rowid NOT IN \
                     (SELECT rowid FROM request_logs ORDER BY rowid DESC LIMIT ?1)",
                    params![REQUEST_LOG_MAX_ROWS],
                )?;
                tx.commit()?;
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

    /// Aggregate request-log metrics since `since_ts` for dashboard charts.
    /// Single table scan — aggregates in memory to avoid six separate GROUP BY passes.
    pub async fn request_log_stats(&self, since_ts: i64) -> AppResult<RequestLogStats> {
        let stats = self
            .conn
            .call(move |conn| {
                let period_days = ((Utc::now().timestamp() - since_ts) / 86_400).max(1) as u32;

                #[derive(Default)]
                struct Agg {
                    count: i64,
                    error_count: i64,
                    duration_sum: i64,
                }

                let mut total = 0i64;
                let mut ok_count = 0i64;
                let mut error_count = 0i64;
                let mut duration_sum = 0i64;
                let mut by_account_map: HashMap<(Option<String>, Option<String>), Agg> =
                    HashMap::new();
                let mut by_operation_map: HashMap<String, Agg> = HashMap::new();
                let mut by_day_map: HashMap<i64, (i64, i64)> = HashMap::new();
                let mut by_day_account_map: HashMap<(i64, Option<String>, Option<String>), i64> =
                    HashMap::new();
                let mut bucket_map: HashMap<String, i64> = HashMap::new();

                let mut stmt = conn.prepare(
                    "SELECT account_id, account_name, operation, bucket, status, duration_ms, created_at
                     FROM request_logs WHERE created_at >= ?1",
                )?;
                let rows = stmt.query_map(params![since_ts], |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, i64>(6)?,
                    ))
                })?;

                for row in rows {
                    let (
                        account_id,
                        account_name,
                        operation,
                        bucket,
                        status,
                        duration_ms,
                        created_at,
                    ) = row?;
                    total += 1;
                    duration_sum += duration_ms;
                    let is_ok = status == "ok";
                    let is_error = status == "error";
                    if is_ok {
                        ok_count += 1;
                    }
                    if is_error {
                        error_count += 1;
                    }

                    let acc = by_account_map
                        .entry((account_id.clone(), account_name.clone()))
                        .or_default();
                    acc.count += 1;
                    if is_error {
                        acc.error_count += 1;
                    }
                    acc.duration_sum += duration_ms;

                    let op = by_operation_map.entry(operation).or_default();
                    op.count += 1;
                    if is_error {
                        op.error_count += 1;
                    }
                    op.duration_sum += duration_ms;

                    let day = (created_at / 86400) * 86400;
                    let day_entry = by_day_map.entry(day).or_insert((0, 0));
                    day_entry.0 += 1;
                    if is_error {
                        day_entry.1 += 1;
                    }

                    *by_day_account_map
                        .entry((day, account_id, account_name))
                        .or_insert(0) += 1;

                    if let Some(b) = bucket.filter(|s| !s.is_empty()) {
                        *bucket_map.entry(b).or_insert(0) += 1;
                    }
                }

                let avg_duration_ms = if total > 0 {
                    duration_sum / total
                } else {
                    0
                };

                let mut by_account: Vec<RequestLogAccountStat> = by_account_map
                    .into_iter()
                    .map(|((account_id, account_name), agg)| RequestLogAccountStat {
                        account_id,
                        account_name,
                        count: agg.count,
                        error_count: agg.error_count,
                        avg_duration_ms: if agg.count > 0 {
                            agg.duration_sum / agg.count
                        } else {
                            0
                        },
                    })
                    .collect();
                by_account.sort_by(|a, b| b.count.cmp(&a.count));

                let mut by_operation: Vec<RequestLogOperationStat> = by_operation_map
                    .into_iter()
                    .map(|(operation, agg)| RequestLogOperationStat {
                        operation,
                        count: agg.count,
                        error_count: agg.error_count,
                        avg_duration_ms: if agg.count > 0 {
                            agg.duration_sum / agg.count
                        } else {
                            0
                        },
                    })
                    .collect();
                by_operation.sort_by(|a, b| b.count.cmp(&a.count));

                let mut by_day: Vec<RequestLogDayStat> = by_day_map
                    .into_iter()
                    .map(|(day, (count, error_count))| RequestLogDayStat {
                        day,
                        count,
                        error_count,
                    })
                    .collect();
                by_day.sort_by_key(|d| d.day);

                let mut by_day_by_account: Vec<RequestLogDayAccountStat> = by_day_account_map
                    .into_iter()
                    .map(|((day, account_id, account_name), count)| RequestLogDayAccountStat {
                        day,
                        account_id,
                        account_name,
                        count,
                    })
                    .collect();
                by_day_by_account.sort_by_key(|d| (d.day, d.account_id.clone(), d.account_name.clone()));

                let mut top_buckets: Vec<RequestLogBucketStat> = bucket_map
                    .into_iter()
                    .map(|(bucket, count)| RequestLogBucketStat { bucket, count })
                    .collect();
                top_buckets.sort_by(|a, b| b.count.cmp(&a.count));
                top_buckets.truncate(10);

                Ok::<_, tokio_rusqlite::Error>(RequestLogStats {
                    period_days,
                    since_ts,
                    total,
                    ok_count,
                    error_count,
                    avg_duration_ms,
                    by_account,
                    by_operation,
                    by_day,
                    by_day_by_account,
                    top_buckets,
                })
            })
            .await?;
        Ok(stats)
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
