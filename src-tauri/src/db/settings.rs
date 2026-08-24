//! Typed key-value settings: one row per key, JSON scalar value. New fields
//! need no migration — extend [`AppSettings`] plus the (de)serialization halves below.

use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};

#[allow(unused_imports)]
use crate::error::{AppError, AppResult};

use super::Db;

/// Defaults baked into [`AppSettings::default`]; the FE patches via `update_settings`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Download-dialog default dir; `None` = OS default. Absolute when set.
    pub default_download_dir: Option<String>,

    /// Max concurrent uploads + downloads; enforced 1..=16.
    pub transfer_concurrency: u32,

    /// Parts of one multipart upload sent in parallel; 1..=16.
    pub multipart_parallelism: u32,

    /// Files above this size use multipart upload (bytes).
    pub multipart_threshold_bytes: u64,

    /// Multipart chunk size; min 5 MiB per S3 spec.
    pub part_size_bytes: u64,

    /// Cached listing TTL; UI shows a stale badge after this elapses.
    pub prefix_sync_ttl_secs: u64,

    /// Presigned URL expiry in seconds; capped at 7 days by the SDK spec.
    pub presign_default_expires_secs: u64,

    /// UI theme. "light" | "dark" | "system".
    pub theme: String,

    /// FE hint to show dot-files; the backend never filters.
    pub show_hidden: bool,

    pub confirm_destructive: bool,

    /// Outbound proxy URL, set as HTTPS_PROXY/HTTP_PROXY before SDK client
    /// build. Changes require app restart (SDK reads env once).
    pub http_proxy: Option<String>,

    /// Custom CA-bundle PEM path, set as SSL_CERT_FILE. Restart required.
    pub custom_ca_path: Option<String>,

    /// Days to retain request-log rows; pruned on startup. 1..=365.
    pub request_log_ttl_days: u32,

    /// Master switch for the local MCP endpoint. Desktop only.
    pub mcp_enabled: bool,

    /// MCP port, bound on 127.0.0.1 only.
    pub mcp_port: u16,

    /// Enables upload/download MCP tools; off = not advertised at all.
    pub mcp_allow_write: bool,

    /// Enables the delete MCP tool; off = not advertised.
    pub mcp_allow_delete: bool,

    /// Reserved for a future per-account allowlist; currently always true.
    pub mcp_bind_all_accounts: bool,

    /// User acknowledged the MCP risk warning; gates the settings UI.
    pub mcp_acknowledged: bool,

    /// Tools turned off individually; not advertised nor dispatched.
    pub mcp_disabled_tools: Vec<String>,

    /// Accounts turned off for MCP; hidden and rejected by tools.
    pub mcp_disabled_accounts: Vec<String>,

    /// MCP transfers are refused unless the local path resolves inside this
    /// dir (object keys are untrusted). None/empty = write tools refuse all.
    pub mcp_fs_root: Option<String>,
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
            request_log_ttl_days: 30,
            mcp_enabled: false,
            mcp_port: 4123,
            mcp_allow_write: false,
            mcp_allow_delete: false,
            mcp_bind_all_accounts: true,
            mcp_acknowledged: false,
            mcp_disabled_tools: Vec::new(),
            mcp_disabled_accounts: Vec::new(),
            mcp_fs_root: None,
        }
    }
}

impl AppSettings {
    /// Clamp numeric fields and trim strings into legal ranges before save.
    fn normalize(&mut self) {
        self.transfer_concurrency = self.transfer_concurrency.clamp(1, 16);
        self.multipart_parallelism = self.multipart_parallelism.clamp(1, 16);
        self.prefix_sync_ttl_secs = self.prefix_sync_ttl_secs.clamp(10, 86400);
        // 5 MiB floor for non-final multipart parts per S3 spec.
        let s3_floor: u64 = 5 * 1024 * 1024;
        self.part_size_bytes = self.part_size_bytes.max(s3_floor);
        self.multipart_threshold_bytes = self.multipart_threshold_bytes.max(s3_floor);
        // 7-day signature ceiling for presigned URLs.
        self.presign_default_expires_secs = self.presign_default_expires_secs.min(7 * 24 * 3600);
        self.request_log_ttl_days = self.request_log_ttl_days.clamp(1, 365);
        self.mcp_port = self.mcp_port.clamp(1024, 65535);
        if !matches!(self.theme.as_str(), "light" | "dark" | "system") {
            self.theme = "system".into();
        }
        if let Some(p) = &self.default_download_dir {
            if p.trim().is_empty() {
                self.default_download_dir = None;
            }
        }
        if let Some(p) = &self.mcp_fs_root {
            if p.trim().is_empty() {
                self.mcp_fs_root = None;
            }
        }
    }
}

impl Db {
    /// Load settings; missing rows fall back to compile-time defaults.
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

    /// Replace the full row-set from `incoming`; keys absent from it stay
    /// untouched. Pair with [`Self::settings_load`] for partial-patch semantics.
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

    /// Reset every known key by deleting all rows.
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

// (De)serialization: stable string keys + JSON scalars. Adding a field =
// extend both halves in lock-step; no DB migration needed.

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
        ("mcp_enabled", enc(&s.mcp_enabled)),
        ("mcp_port", enc(&s.mcp_port)),
        ("mcp_allow_write", enc(&s.mcp_allow_write)),
        ("mcp_allow_delete", enc(&s.mcp_allow_delete)),
        ("mcp_bind_all_accounts", enc(&s.mcp_bind_all_accounts)),
        ("mcp_acknowledged", enc(&s.mcp_acknowledged)),
        ("mcp_disabled_tools", enc(&s.mcp_disabled_tools)),
        ("mcp_disabled_accounts", enc(&s.mcp_disabled_accounts)),
        ("mcp_fs_root", enc(&s.mcp_fs_root)),
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
        "mcp_enabled" => {
            if let Some(v) = dec(raw) {
                s.mcp_enabled = v;
            }
        }
        "mcp_port" => {
            if let Some(v) = dec(raw) {
                s.mcp_port = v;
            }
        }
        "mcp_allow_write" => {
            if let Some(v) = dec(raw) {
                s.mcp_allow_write = v;
            }
        }
        "mcp_allow_delete" => {
            if let Some(v) = dec(raw) {
                s.mcp_allow_delete = v;
            }
        }
        "mcp_bind_all_accounts" => {
            if let Some(v) = dec(raw) {
                s.mcp_bind_all_accounts = v;
            }
        }
        "mcp_acknowledged" => {
            if let Some(v) = dec(raw) {
                s.mcp_acknowledged = v;
            }
        }
        "mcp_disabled_tools" => {
            if let Some(v) = dec::<Vec<String>>(raw) {
                s.mcp_disabled_tools = v;
            }
        }
        "mcp_disabled_accounts" => {
            if let Some(v) = dec::<Vec<String>>(raw) {
                s.mcp_disabled_accounts = v;
            }
        }
        "mcp_fs_root" => {
            if let Some(v) = dec::<Option<String>>(raw) {
                s.mcp_fs_root = v;
            }
        }
        // Unknown keys silently ignored so older binaries tolerate newer rows.
        _ => {}
    }
}


/// Applies proxy/CA env vars; must run before any SDK client is built.
/// SAFETY: `set_var` races `getenv`, but both call sites run before any reader exists.
pub fn apply_network_env(settings: &AppSettings) {
    if let Some(proxy) = settings.http_proxy.as_deref() {
        if !proxy.trim().is_empty() {
            unsafe {
                std::env::set_var("HTTPS_PROXY", proxy);
                std::env::set_var("HTTP_PROXY", proxy);
            }
        }
    }
    if let Some(ca) = settings.custom_ca_path.as_deref() {
        if !ca.trim().is_empty() {
            unsafe {
                std::env::set_var("SSL_CERT_FILE", ca);
            }
        }
    }
}
