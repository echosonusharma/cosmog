//! Cosmog backend — desktop client for S3-compatible object stores.
//!
//! High-level layering:
//!
//! ```text
//! Tauri commands (commands/*)
//!         │
//!         ▼
//! AppState ── TransferManager ── persistent queue (db/transfers)
//!         │            │
//!         ▼            ▼
//!  ObjectStore trait (store/mod) ── S3Store (store/s3) ── aws-sdk-s3
//!         │
//!         ▼
//!  Account configs (db/accounts) + OS keyring (secrets)
//! ```
//!
//! Adding a new provider:
//! 1. Create `store/<name>.rs` implementing [`store::ObjectStore`].
//! 2. Add a variant to [`providers::Protocol`] and a branch in
//!    [`providers::build_store`].
//!
//! Adding new persistent schema: append a `Migration` to the migrations list
//! in `db/mod.rs`. Never edit or reorder existing entries.

// Modules are exposed pub so the integration-tests crate (under tests/) can
// reach into the backend internals. None of these are FE-callable; only the
// `commands::*` functions actually serve as the Tauri API surface.
pub mod bulk;
pub mod commands;
pub mod crypto;
pub mod db;
pub mod error;
pub mod providers;
pub mod scheduler;
pub mod saf;
pub mod secrets;
pub mod state;
pub mod store;
pub mod sync;
pub mod transfer;
pub mod validate;

use tauri::Manager;

use crate::db::Db;
use crate::state::AppState;

#[cfg(debug_assertions)]
#[tauri::command]
fn open_devtools(window: tauri::WebviewWindow) {
    window.open_devtools();
}

/// Native notification command. Uses tauri-plugin-notification's builder so we
/// can pass an Android drawable name for the icon and a stable id so subsequent
/// calls with the same id REPLACE the existing notification instead of stacking.
#[tauri::command]
fn notify_ex(
    app: tauri::AppHandle,
    id: i32,
    title: String,
    body: Option<String>,
    icon: Option<String>,
    ongoing: Option<bool>,
    auto_cancel: Option<bool>,
    silent: Option<bool>,
    channel_id: Option<String>,
    action_type_id: Option<String>,
    summary: Option<String>,
    large_body: Option<String>,
    extra: Option<std::collections::HashMap<String, serde_json::Value>>,
) -> Result<(), String> {
    use tauri_plugin_notification::NotificationExt;
    let mut b = app.notification().builder().id(id).title(title);
    if let Some(v) = body { b = b.body(v); }
    if let Some(v) = summary { b = b.summary(v); }
    if let Some(v) = large_body { b = b.large_body(v); }
    if let Some(v) = icon { b = b.icon(v); }
    if let Some(v) = channel_id { b = b.channel_id(v); }
    if let Some(v) = action_type_id { b = b.action_type_id(v); }
    if let Some(map) = extra {
        for (k, v) in map { b = b.extra(k, v); }
    }
    if ongoing.unwrap_or(false) { b = b.ongoing(); }
    if auto_cancel.unwrap_or(false) { b = b.auto_cancel(); }
    if silent.unwrap_or(false) { b = b.silent(); }
    b.show().map_err(|e| e.to_string())
}

/// Stream a completed download from an absolute cache path into a SAF
/// content:// URI. Chunked so multi-GB files never load fully into memory.
/// See `saf.rs` for the JNI implementation.
#[tauri::command]
async fn finalize_saf_download(cache_path: String, uri: String) -> Result<u64, String> {
    crate::saf::finalize_saf_download(cache_path, uri).await
}

/// Delete the SAF placeholder document created by the save dialog. Called
/// when a download is canceled or fails so no 0-byte file is left at the
/// user's chosen destination.
#[tauri::command]
async fn delete_saf_document(uri: String) -> Result<bool, String> {
    crate::saf::delete_saf_document(uri).await
}

/// Stream a SAF `content://` URI into the app cache and return a filesystem
/// path the uploader can use. Also returns the human display name (from
/// ContentResolver's OpenableColumns.DISPLAY_NAME) for use as the S3 key.
#[tauri::command]
async fn stage_saf_upload(
    uri: String,
    dest_dir: String,
) -> Result<crate::saf::SafStagedUpload, String> {
    crate::saf::stage_saf_upload(uri, dest_dir).await
}

/// Toggle the Android foreground TransferService. FE polling calls this with
/// `active=true` when there is at least one in-flight transfer, and
/// `active=false` when the queue drains. No-op on non-Android platforms.
#[tauri::command]
fn set_transfer_service(active: bool) -> Result<(), String> {
    crate::saf::set_transfer_service(active)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .setup(|app| {
            use tracing_subscriber::layer::SubscriberExt;
            use tracing_subscriber::util::SubscriberInitExt;
            use tracing_subscriber::Layer;

            let app_dir = app.path().app_data_dir().expect("resolve app data dir");

            // If the user requested a full data wipe (via clear_app_data command),
            // a marker file is written before exit. Apply it now, before any
            // files in app_dir are opened, so nothing is held open during removal.
            let wipe_marker = app_dir.join("pending_wipe");
            if wipe_marker.exists() {
                match std::fs::remove_dir_all(&app_dir) {
                    Ok(()) => {
                        let _ = std::fs::create_dir_all(&app_dir);
                    }
                    Err(e) => {
                        // Wipe failed (permissions, locked file, etc.). Remove the
                        // marker so the app doesn't retry on every boot with partial
                        // data present. User can retry the clear via Settings.
                        eprintln!("pending_wipe: remove_dir_all failed: {e}");
                        let _ = std::fs::remove_file(&wipe_marker);
                    }
                }
            }

            let db_path = app_dir.join("cosmog.sqlite");
            let log_dir = app_dir.join("logs");

            std::fs::create_dir_all(&log_dir).ok();
            let file_appender = tracing_appender::rolling::daily(&log_dir, "cosmog.log");
            let (file_writer, guard) = tracing_appender::non_blocking(file_appender);
            // Keep guard alive for the lifetime of the process so the
            // non-blocking writer flushes its queue on clean shutdown.
            static LOG_GUARD: std::sync::OnceLock<tracing_appender::non_blocking::WorkerGuard> =
                std::sync::OnceLock::new();
            let _ = LOG_GUARD.set(guard);

            let env_filter = || {
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
            };

            // Both layers registered in one registry so neither silently loses.
            let console_layer = tracing_subscriber::fmt::layer().with_filter(env_filter());
            let file_layer = tracing_subscriber::fmt::layer()
                .with_writer(file_writer)
                .with_ansi(false)
                .with_filter(env_filter());

            tracing_subscriber::registry()
                .with(console_layer)
                .with(file_layer)
                .init();

            let handle = app.handle().clone();
            tauri::async_runtime::block_on(async move {
                // Apply any pending restore staged by the user in a previous
                // session. Re-validate the file is still a SQLite DB right
                // before the swap — paranoia in case the file was tampered
                // with between staging and boot.
                let pending = db_path.with_extension("restore_pending");
                if pending.exists() {
                    let valid = match tokio::fs::File::open(&pending).await {
                        Ok(mut f) => {
                            use tokio::io::AsyncReadExt;
                            let mut header = [0u8; 16];
                            f.read_exact(&mut header).await.is_ok()
                                && &header == b"SQLite format 3\0"
                        }
                        Err(_) => false,
                    };
                    if !valid {
                        tracing::warn!(
                            "restore_pending at {} is not a SQLite DB; ignoring + removing",
                            pending.display()
                        );
                        let _ = tokio::fs::remove_file(&pending).await;
                    } else if let Err(e) = tokio::fs::rename(&pending, &db_path).await {
                        tracing::warn!("apply restore_pending failed: {e}");
                    } else {
                        tracing::info!("applied pending restore to {}", db_path.display());
                    }
                }

                let db = Db::open(&db_path).await.expect("open db");
                // Reap any transfers left as Active/Pending by a previous
                // crash so the UI doesn't show ghost-running rows.
                if let Err(e) = db.reap_orphan_transfers().await {
                    tracing::warn!("reap_orphan_transfers failed: {e}");
                }
                // Honour user-configured concurrency at startup. Note: changes
                // made via update_settings only take effect on next launch
                // because the Semaphore is not resizable in place.
                let settings = db.settings_load().await.unwrap_or_default();
                // Apply network env from settings BEFORE the SDK client is
                // ever constructed. Takes effect on next call (SDK reads env
                // at builder time).
                // `set_var` is `unsafe` on Rust 1.80+ because env mutation is
                // process-global and could race with other threads reading
                // env. We run this once at boot before the SDK client (or
                // anyone else) reads the relevant variables, so the race
                // window is zero in practice.
                if let Some(proxy) = &settings.http_proxy {
                    if !proxy.trim().is_empty() {
                        // SAFETY: see comment above.
                        unsafe {
                            std::env::set_var("HTTPS_PROXY", proxy);
                            std::env::set_var("HTTP_PROXY", proxy);
                        }
                    }
                }
                if let Some(ca) = &settings.custom_ca_path {
                    if !ca.trim().is_empty() {
                        // SAFETY: see comment above.
                        unsafe {
                            std::env::set_var("SSL_CERT_FILE", ca);
                        }
                    }
                }
                let concurrency = settings.transfer_concurrency as usize;
                // Prune request logs older than the configured TTL.
                let ttl_cutoff = chrono::Utc::now().timestamp()
                    - (settings.request_log_ttl_days as i64 * 86_400);
                if let Err(e) = db.delete_old_request_logs(ttl_cutoff).await {
                    tracing::warn!("request log TTL cleanup failed: {e}");
                }
                // Sweep leftover encryption temp files from a previous crash.
                // Files under <db_dir>/enc_tmp/ are ciphertext-only, safe to
                // delete unconditionally: they were staged for uploads that
                // never completed.
                if let Some(parent) = db_path.parent() {
                    let enc_tmp = parent.join("enc_tmp");
                    if enc_tmp.exists() {
                        match std::fs::read_dir(&enc_tmp) {
                            Ok(rd) => {
                                let mut removed = 0usize;
                                for entry in rd.flatten() {
                                    if std::fs::remove_file(entry.path()).is_ok() {
                                        removed += 1;
                                    }
                                }
                                if removed > 0 {
                                    tracing::info!(
                                        "swept {} stale file(s) from {}",
                                        removed,
                                        enc_tmp.display()
                                    );
                                }
                            }
                            Err(e) => tracing::warn!("enc_tmp sweep failed: {e}"),
                        }
                    }
                }
                let state = AppState::new(db, concurrency, log_dir, db_path, app.handle().clone());
                // Keep the cancel token alive for the process lifetime so the
                // scheduler can be stopped cleanly if needed. Stored in a
                // OnceLock so it is not dropped until the process exits.
                static SCHEDULER_CANCEL: std::sync::OnceLock<tokio_util::sync::CancellationToken> =
                    std::sync::OnceLock::new();
                let _ = SCHEDULER_CANCEL.set(scheduler::spawn(state.clone()));
                handle.manage(state);
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // -------- accounts: provider credentials + connection state --------
            commands::accounts::add_account,                // insert account row + stash secret in OS keyring
            commands::accounts::list_accounts,              // return all configured accounts (metadata only, no secrets)
            commands::accounts::get_account,                // fetch one account by id
            commands::accounts::update_account,             // patch fields; optional secret rotation in keyring
            commands::accounts::delete_account,             // cancel in-flight transfers/scans, drop row + secret + cached client
            commands::accounts::test_account,               // connectivity probe (calls list_buckets)
            commands::accounts::detect_account_region,      // ask the bucket for its actual region, update stored value if it differs

            // -------- buckets: top-level container ops --------
            commands::buckets::list_buckets,                // enumerate all buckets visible to the credentials
            commands::buckets::create_bucket,               // make a new bucket, optionally pinning a region
            commands::buckets::delete_bucket,               // remove an (empty) bucket
            commands::buckets::head_bucket,                 // existence + access check
            commands::buckets::get_bucket_location,         // read the bucket's actual region
            commands::buckets::put_bucket_acl,              // set canned ACL (private / public-read)
            commands::buckets::get_bucket_versioning,       // is versioning currently enabled?
            commands::buckets::put_bucket_versioning,       // toggle versioning on/off
            commands::buckets::list_multipart_uploads,      // list in-progress multipart uploads (paged)
            commands::buckets::cleanup_stale_multiparts,    // abort any multipart older than N seconds — stops leaked-part cost
            commands::buckets::abort_multipart_upload,      // abort one specific upload by id

            // -------- objects: single-object ops (metadata-only paths) --------
            commands::objects::list_objects,                // raw S3 listing pass-through (paged, prefix/delimiter)
            commands::objects::head_object,                 // fetch metadata + refresh local cache row
            commands::objects::create_folder,               // put zero-byte object with trailing slash
            commands::objects::delete_object,               // delete one key + remove cache row
            commands::objects::delete_objects,              // batch delete up to 1000 keys per call
            commands::objects::delete_object_version,       // delete a specific version (versioned buckets)
            commands::objects::list_object_versions,        // list versions + delete markers under a prefix
            commands::objects::copy_object,                 // server-side copy + cache write-through
            commands::objects::move_object,                 // copy-then-delete; cache mirror across both sides
            commands::objects::put_object_acl,              // set object-level canned ACL
            commands::objects::get_object_tagging,          // read object tag set (AWS only)
            commands::objects::put_object_tagging,          // set object tag set (AWS only)
            commands::objects::delete_object_tagging,       // clear all tags on object (AWS only)
            commands::objects::presign_get,                 // generate time-limited presigned GET URL
            commands::objects::preview_object,              // in-memory read of first N bytes for FE previews
            commands::objects::put_object_text,             // save edited text content directly without temp file
            commands::objects::put_object_bytes_cmd,        // save binary content (e.g. xlsx) directly
            commands::objects::list_keys_under_prefix,      // live S3 listing of all keys under prefix (no cache)

            // -------- transfers: persistent upload/download queue --------
            commands::transfers::enqueue_upload,            // queue an upload (returns transfer_id; events via Channel)
            commands::transfers::enqueue_download,          // queue a download (returns transfer_id; events via Channel)
            commands::transfers::list_transfers,            // list transfer rows, optionally filtered by status
            commands::transfers::get_transfer,              // fetch one transfer row by id
            commands::transfers::cancel_transfer,           // signal cancel; idempotent if already terminal
            commands::transfers::retry_transfer,            // re-enqueue with original options + multipart resume
            commands::transfers::clear_completed_transfers, // delete done/failed/canceled rows from history
            commands::transfers::clear_transfer,            // delete one transfer row by id

            // -------- search: cached-object FTS + faceted browse --------
            commands::search::search_objects,               // FTS5 query + facets over the local cache
            commands::search::sync_prefix,                  // refresh cache for one prefix (direct or recursive)
            commands::search::bucket_index_status,          // is full-bucket index enabled? when last synced?
            commands::search::enable_bucket_index,          // turn on indexing + run/resume initial full scan
            commands::search::cancel_bucket_scan,           // stop an in-flight full scan (resumable later)
            commands::search::reindex_bucket,               // fresh full scan, discarding any resume token
            commands::search::disable_bucket_index,         // drop cache + turn off indexing for a bucket
            commands::search::bucket_stats,                 // aggregate object count + total bytes + storage-class breakdown
            commands::search::set_bucket_auto_reindex,      // configure scheduler to re-scan every N seconds

            // -------- settings: user preferences --------
            commands::settings::get_settings,               // load current AppSettings (defaults if unset)
            commands::settings::update_settings,            // partial-patch update with normalization
            commands::settings::reset_settings,             // wipe settings row, return defaults

            // -------- bulk: folder-scoped multi-object operations --------
            commands::bulk::delete_folder_cmd,              // recursive list + batched DeleteObjects under a prefix
            commands::bulk::upload_directory_cmd,           // walk a local dir, enqueue every file as a separate upload
            commands::bulk::download_directory_cmd,         // walk a remote prefix, enqueue every key as a separate download
            commands::bulk::cancel_bulk_op,                 // cancel an in-flight bulk op by its op_id

            // -------- capabilities: permission probing --------
            commands::capabilities::probe_account_capabilities, // probe list_buckets at the account level
            commands::capabilities::probe_bucket_capabilities,  // probe head/list/versioning/location for one bucket
            commands::capabilities::get_account_capabilities,   // read cached account-level probe result
            commands::capabilities::get_bucket_capabilities,    // read cached bucket-level probe result + write-attempt log

            // -------- logs: diagnostic file access --------
            commands::logs::get_log_dir,                    // return path to the rolling log directory
            commands::logs::get_log_tail,                   // return last N bytes of today's log file

            // -------- request_logs: S3 API call history --------
            commands::request_logs::list_request_logs,      // paginated S3 API request log (newest first)
            commands::request_logs::count_request_logs,     // count of log rows matching optional search
            commands::request_logs::clear_request_logs,     // delete all request log rows
            commands::request_logs::purge_old_request_logs, // delete rows older than the TTL setting

            // -------- portable: backup / restore / import / export --------
            commands::portable::export_config,              // dump accounts (no secrets) + settings as a JSON bundle
            commands::portable::import_config,              // merge a JSON bundle into the local DB
            commands::portable::backup_database,            // atomic SQLite-Backup-API copy of the live DB to a path
            commands::portable::stage_restore,              // validate + stage a SQLite file; applied at next boot
            commands::portable::clear_app_data,             // delete all keyring secrets + wipe app data dir on next boot, then exit

            // -------- encryption: per-bucket age (X25519 + ChaCha20-Poly1305) --------
            commands::encryption::enable_bucket_encryption,   // generate identity, store secret in keychain, record recipient in DB
            commands::encryption::disable_bucket_encryption,  // remove identity from keychain + config from DB
            commands::encryption::get_bucket_encryption_status, // is encryption enabled for this bucket?
            commands::encryption::export_encryption_key,      // return identity payload for external decryption tools
            commands::encryption::save_encryption_key_export,  // write identity file (0600) to a user-chosen path
            commands::encryption::import_encryption_identity,  // load identity text back into the keychain (recovery)
            commands::encryption::import_encryption_identity_from_file, // convenience: read + import
            commands::encryption::has_encryption_identity,     // FE preflight: keychain-present check
            commands::encryption::list_encrypted_buckets,      // per-account list of encrypted buckets for grid lock badges

            // -------- browse: cache-aware navigation --------
            commands::browse::browse_prefix,                // return cached children + sub-prefixes; background-refresh if stale

            // -------- notifications: native builder with icon + stable id --------
            notify_ex,

            // -------- Android SAF: stream SAF URIs into/out of app cache without loading files into JS --------
            finalize_saf_download,
            delete_saf_document,
            stage_saf_upload,
            set_transfer_service,

            // -------- dev: debug helpers --------
            #[cfg(debug_assertions)]
            open_devtools,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
