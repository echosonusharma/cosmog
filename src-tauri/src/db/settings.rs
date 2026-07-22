//! Application-wide user preferences.
//!
//! Stored as a typed key-value table so new settings can be added without a
//! schema migration — just extend [`AppSettings`] and the (de)serialization
//! helpers below. The on-disk format is one row per `key`, with `value`
//! holding a JSON-encoded scalar (string, integer, boolean).
//!
//! [`AppSettings::load`] reads every known key, falling back to compile-time
//! defaults when a row is missing; [`AppSettings::save`] writes every field
//! back. Consumers should treat the struct as the canonical source of truth
//! and not poke at the raw table directly.

use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};

#[allow(unused_imports)]
use crate::error::{AppError, AppResult};

use super::Db;

/// User preferences and tunables.
///
/// Defaults are baked into [`AppSettings::default`]; the FE can override any
/// subset by sending a partial patch through the `update_settings` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Filesystem directory pre-selected in download dialogs. `None` = OS
    /// default ("Downloads"). Must be absolute when set.
    pub default_download_dir: Option<String>,

    /// Maximum number of concurrent uploads + downloads. Higher = faster
    /// throughput, more memory + bandwidth. Range enforced 1..=16.
    pub transfer_concurrency: u32,

    /// How many parts of a single multipart upload to send in parallel.
    /// Range 1..=16.
    pub multipart_parallelism: u32,

    /// Files above this size use multipart upload. Below uses single PUT.
    /// In bytes; minimum is the S3 floor (5 MiB) for non-final parts.
    pub multipart_threshold_bytes: u64,

    /// Size of each multipart chunk. Minimum 5 MiB per S3 spec.
    pub part_size_bytes: u64,

    /// How long a cached prefix listing is considered fresh. The UI shows a
    /// "stale" badge after this elapses and may auto-refresh.
    pub prefix_sync_ttl_secs: u64,

    /// Default expiry for presigned URLs in seconds. Capped at 7 days
    /// (604800) by the SDK signature spec.
    pub presign_default_expires_secs: u64,

    /// UI theme. "light" | "dark" | "system".
    pub theme: String,

    /// Whether the FE should show objects whose key starts with `.`
    /// (hidden-file convention). Backend doesn't filter — it's a UI hint.
    pub show_hidden: bool,

    /// Show a confirmation dialog before destructive ops (delete, overwrite).
    pub confirm_destructive: bool,

    /// Outbound HTTP/HTTPS proxy URL. Honoured by setting `HTTPS_PROXY` /
    /// `HTTP_PROXY` env vars before constructing the SDK client. Empty / None
    /// = no proxy. Reverted on settings change requires app restart because
    /// the AWS SDK reads env once at client build.
    pub http_proxy: Option<String>,

    /// Filesystem path to a custom CA-bundle PEM. Honoured by setting
    /// `SSL_CERT_FILE` env var. Reverted requires app restart.
    pub custom_ca_path: Option<String>,

    /// How many days to retain API request log entries. Older rows are
    /// deleted on startup. Range enforced 1..=365.
    pub request_log_ttl_days: u32,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            default_download_dir: None,
            transfer_concurrency: 3,
            multipart_parallelism: 4,
            multipart_threshold_bytes: 8 * 1024 * 1024,
            part_size_bytes: 8 * 1024 * 1024,
            prefix_sync_ttl_secs: 300,
            presign_default_expires_secs: 3600,
            theme: "system".into(),
            show_hidden: false,
            confirm_destructive: true,
            http_proxy: None,
            custom_ca_path: None,
            request_log_ttl_days: 14,
        }
    }
}

impl AppSettings {
    /// Clamp numeric fields and trim string fields into legal ranges. Called
    /// before any save so an out-of-range FE value can't poison the DB.
    fn normalize(&mut self) {
        self.transfer_concurrency = self.transfer_concurrency.clamp(1, 16);
        self.multipart_parallelism = self.multipart_parallelism.clamp(1, 16);
        // 10s floor so the app never hammers the server; 24h ceiling is generous.
        self.prefix_sync_ttl_secs = self.prefix_sync_ttl_secs.clamp(10, 86400);
        // 5 MiB floor for non-final multipart parts per S3 spec.
        let s3_floor: u64 = 5 * 1024 * 1024;
        self.part_size_bytes = self.part_size_bytes.max(s3_floor);
        self.multipart_threshold_bytes = self.multipart_threshold_bytes.max(s3_floor);
        // 7-day signature ceiling for presigned URLs.
        self.presign_default_expires_secs = self.presign_default_expires_secs.min(7 * 24 * 3600);
        self.request_log_ttl_days = self.request_log_ttl_days.clamp(1, 365);
        if !matches!(self.theme.as_str(), "light" | "dark" | "system") {
            self.theme = "system".into();
        }
        if let Some(p) = &self.default_download_dir {
            if p.trim().is_empty() {
                self.default_download_dir = None;
            }
        }
    }
}

impl Db {
    /// Load all known settings. Missing keys fall back to defaults; the
    /// caller never has to handle absence.
    pub async fn settings_load(&self) -> AppResult<AppSettings> {
        let rows = self
            .conn
            .call(|conn| {
                let mut stmt = conn.prepare("SELECT key, value FROM settings")?;
                let iter = stmt.query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?;
                let mut out: Vec<(String, String)> = Vec::new();
                for r in iter {
                    out.push(r?);
                }
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await?;

        let mut s = AppSettings::default();
        for (key, raw) in rows {
            apply_setting(&mut s, &key, &raw);
        }
        Ok(s)
    }

    /// Replace the full settings row-set with the values in `incoming`. Any
    /// key absent from `incoming` is left untouched in the DB; combining with
    /// [`Self::settings_load`] in the command layer gives partial-patch
    /// semantics safely.
    pub async fn settings_save(&self, mut incoming: AppSettings) -> AppResult<AppSettings> {
        incoming.normalize();
        let pairs = serialize_settings(&incoming);
        let now = Utc::now().timestamp();

        self.conn
            .call(move |conn| {
                let tx = conn.transaction()?;
                for (k, v) in &pairs {
                    tx.execute(
                        "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)
                         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                        params![k, v, now],
                    )?;
                }
                tx.commit()?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(incoming)
    }

    /// Reset every known key by deleting all setting rows. Subsequent loads
    /// return [`AppSettings::default`].
    pub async fn settings_reset(&self) -> AppResult<AppSettings> {
        self.conn
            .call(|conn| {
                conn.execute("DELETE FROM settings", [])?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
        Ok(AppSettings::default())
    }
}

// ---------------------------------------------------------------------------
// Internal (de)serialization. Each known field has a stable string key + a
// JSON-encoded scalar for the value. Adding a new field = extend both halves
// in lock-step; no DB migration needed.
// ---------------------------------------------------------------------------

fn serialize_settings(s: &AppSettings) -> Vec<(&'static str, String)> {
    fn enc<T: serde::Serialize>(v: &T) -> String {
        serde_json::to_string(v).unwrap_or_else(|_| "null".into())
    }
    vec![
        ("default_download_dir", enc(&s.default_download_dir)),
        ("transfer_concurrency", enc(&s.transfer_concurrency)),
        ("multipart_parallelism", enc(&s.multipart_parallelism)),
        ("multipart_threshold_bytes", enc(&s.multipart_threshold_bytes)),
        ("part_size_bytes", enc(&s.part_size_bytes)),
        ("prefix_sync_ttl_secs", enc(&s.prefix_sync_ttl_secs)),
        ("presign_default_expires_secs", enc(&s.presign_default_expires_secs)),
        ("theme", enc(&s.theme)),
        ("show_hidden", enc(&s.show_hidden)),
        ("confirm_destructive", enc(&s.confirm_destructive)),
        ("http_proxy", enc(&s.http_proxy)),
        ("custom_ca_path", enc(&s.custom_ca_path)),
        ("request_log_ttl_days", enc(&s.request_log_ttl_days)),
    ]
}

fn apply_setting(s: &mut AppSettings, key: &str, raw: &str) {
    fn dec<T: serde::de::DeserializeOwned>(raw: &str) -> Option<T> {
        serde_json::from_str(raw).ok()
    }
    match key {
        "default_download_dir" => {
            if let Some(v) = dec::<Option<String>>(raw) {
                s.default_download_dir = v;
            }
        }
        "transfer_concurrency" => {
            if let Some(v) = dec(raw) {
                s.transfer_concurrency = v;
            }
        }
        "multipart_parallelism" => {
            if let Some(v) = dec(raw) {
                s.multipart_parallelism = v;
            }
        }
        "multipart_threshold_bytes" => {
            if let Some(v) = dec(raw) {
                s.multipart_threshold_bytes = v;
            }
        }
        "part_size_bytes" => {
            if let Some(v) = dec(raw) {
                s.part_size_bytes = v;
            }
        }
        "prefix_sync_ttl_secs" => {
            if let Some(v) = dec(raw) {
                s.prefix_sync_ttl_secs = v;
            }
        }
        "presign_default_expires_secs" => {
            if let Some(v) = dec(raw) {
                s.presign_default_expires_secs = v;
            }
        }
        "theme" => {
            if let Some(v) = dec(raw) {
                s.theme = v;
            }
        }
        "show_hidden" => {
            if let Some(v) = dec(raw) {
                s.show_hidden = v;
            }
        }
        "confirm_destructive" => {
            if let Some(v) = dec(raw) {
                s.confirm_destructive = v;
            }
        }
        "http_proxy" => {
            if let Some(v) = dec(raw) {
                s.http_proxy = v;
            }
        }
        "custom_ca_path" => {
            if let Some(v) = dec(raw) {
                s.custom_ca_path = v;
            }
        }
        "request_log_ttl_days" => {
            if let Some(v) = dec(raw) {
                s.request_log_ttl_days = v;
            }
        }
        // Unknown key — silently ignored so older binaries don't corrupt
        // settings rows written by newer ones.
        _ => {}
    }
}

