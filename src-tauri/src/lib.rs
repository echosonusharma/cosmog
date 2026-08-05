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
#[cfg(not(target_os = "android"))]
pub mod app_lifecycle;
pub mod bulk;
pub mod commands;
pub mod crypto;
pub mod db;
pub mod device;
pub mod error;
#[cfg(not(target_os = "android"))]
pub mod mcp;
pub mod night_watcher;
#[cfg(target_os = "android")]
pub mod night_watcher_headless;
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

/// Real platform info for the bug-report dialog: OS name + version + CPU arch
/// (and device model on Android). Resolved natively so it reflects the device,
/// not the WebView's `navigator` string.
#[tauri::command]
fn get_device_info() -> Result<crate::device::DeviceInfo, String> {
    crate::device::get_device_info()
}

/// Guaranteed quit path for background running. Called by the FE quit button,
/// primarily for no-tray hosts (e.g. Wayland) where there is no tray Quit item.
/// Marks quit as requested so the close-guard steps aside, then exits.
#[cfg(not(target_os = "android"))]
#[tauri::command]
fn nw_quit_background(app: tauri::AppHandle) {
    app_lifecycle::request_quit();
    app.exit(0);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();

    // Desktop-only plugins, registered FIRST. single-instance must be the very
    // first plugin so a second launch is routed to the running instance before
    // any other plugin initializes. autostart is launched hidden via --hidden.
    #[cfg(not(target_os = "android"))]
    let builder = builder
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // A second launch focuses the existing window instead of opening a
            // new process.
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ));

    builder
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
                // Reap transfers left Active/Pending by a previous crash so the
                // UI doesn't show ghost-running rows. Desktop only: on Android
                // the sibling :nightwatch process may hold genuinely-live rows,
                // and this blind UPDATE has no owner column to spare them.
                #[cfg(not(target_os = "android"))]
                if let Err(e) = db.reap_orphan_transfers().await {
                    tracing::warn!("reap_orphan_transfers failed: {e}");
                }
                // Honour user-configured concurrency at startup. Note: changes
                // made via update_settings only take effect on next launch
                // because the Semaphore is not resizable in place.
                let settings = db.settings_load().await.unwrap_or_default();
                // Apply proxy / custom-CA env BEFORE any SDK client is built.
                crate::db::settings::apply_network_env(&settings);
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
                // Night Watcher: background local-dir -> S3 sync. Sibling of the
                // scheduler; its cancel token is likewise parked for the process
                // lifetime.
                //
                // Android: the sync loop runs in the separate `:nightwatch`
                // service process (night_watcher_headless), NOT here. wry exits
                // this process when the Activity is destroyed, and a second loop
                // in this process would double-scan + race the service's writes
                // to nw_file_state. The main process only edits watch config.
                #[cfg(not(target_os = "android"))]
                {
                    static NIGHT_WATCHER_CANCEL: std::sync::OnceLock<
                        tokio_util::sync::CancellationToken,
                    > = std::sync::OnceLock::new();
                    let _ = NIGHT_WATCHER_CANCEL.set(night_watcher::spawn(state.clone()));
                }

                // Arm/disarm desktop background running based on whether any
                // watch is enabled, then let the SAF twin start/stop the
                // Android FGS (no-op on desktop).
                #[cfg(not(target_os = "android"))]
                {
                    let enabled = state
                        .db
                        .list_enabled_watches()
                        .await
                        .map(|v| !v.is_empty())
                        .unwrap_or(false);
                    app_lifecycle::apply(app.handle(), enabled);
                }
                // MCP server: local Streamable HTTP endpoint. Starts only when
                // enabled in settings; shares the one AppState with everything
                // else. Its enabled flag also feeds the background-run gate.
                #[cfg(not(target_os = "android"))]
                if let Err(e) = mcp::apply(&state).await {
                    tracing::warn!("MCP server start failed: {e}");
                }
                commands::night_watcher::nw_refresh_service(&state).await;

                handle.manage(state);

                // Show the window unless launched hidden (autostart passes
                // --hidden). With visible:false in the config, doing nothing
                // keeps it hidden.
                #[cfg(not(target_os = "android"))]
                if !std::env::args().any(|a| a == "--hidden") {
                    if let Some(w) = app.get_webview_window("main") {
                        let _ = w.show();
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // -------- accounts: provider credentials + connection state --------
            commands::accounts::add_account,
            commands::accounts::list_accounts,
            commands::accounts::get_account,
            commands::accounts::update_account,
            commands::accounts::delete_account,
            commands::accounts::test_account,
            commands::accounts::detect_account_region,

            // -------- buckets: top-level container ops --------
            commands::buckets::list_buckets,
            commands::buckets::create_bucket,
            commands::buckets::delete_bucket,
            commands::buckets::head_bucket,
            commands::buckets::get_bucket_location,
            commands::buckets::put_bucket_acl,
            commands::buckets::get_bucket_versioning,
            commands::buckets::put_bucket_versioning,
            commands::buckets::get_bucket_policy,
            commands::buckets::put_bucket_policy,
            commands::buckets::delete_bucket_policy,
            commands::buckets::get_bucket_cors,
            commands::buckets::put_bucket_cors,
            commands::buckets::delete_bucket_cors,
            commands::buckets::list_multipart_uploads,
            commands::buckets::cleanup_stale_multiparts,
            commands::buckets::abort_multipart_upload,

            // -------- objects: single-object ops (metadata-only paths) --------
            commands::objects::list_objects,
            commands::objects::head_object,
            commands::objects::create_folder,
            commands::objects::delete_object,
            commands::objects::delete_objects,
            commands::objects::delete_object_version,
            commands::objects::restore_object_version,
            commands::objects::list_object_versions,
            commands::objects::copy_object,
            commands::objects::move_object,
            commands::objects::put_object_acl,
            commands::objects::get_object_tagging,
            commands::objects::put_object_tagging,
            commands::objects::delete_object_tagging,
            commands::objects::presign_get,
            commands::objects::preview_object,
            commands::objects::put_object_text,
            commands::objects::put_object_bytes_cmd,
            commands::objects::list_keys_under_prefix,

            // -------- transfers: persistent upload/download queue --------
            commands::transfers::enqueue_upload,
            commands::transfers::enqueue_download,
            commands::transfers::list_transfers,
            commands::transfers::get_transfer,
            commands::transfers::cancel_transfer,
            commands::transfers::retry_transfer,
            commands::transfers::clear_completed_transfers,
            commands::transfers::clear_transfer,

            // -------- search: cached-object FTS + faceted browse --------
            commands::search::search_objects,
            commands::search::sync_prefix,
            commands::search::bucket_index_status,
            commands::search::enable_bucket_index,
            commands::search::cancel_bucket_scan,
            commands::search::reindex_bucket,
            commands::search::disable_bucket_index,
            commands::search::bucket_stats,
            commands::search::set_bucket_auto_reindex,

            // -------- settings: user preferences --------
            commands::settings::get_settings,
            commands::settings::update_settings,
            commands::settings::reset_settings,

            // -------- bulk: folder-scoped multi-object operations --------
            commands::bulk::delete_folder_cmd,
            commands::bulk::upload_directory_cmd,
            commands::bulk::download_directory_cmd,
            commands::bulk::cancel_bulk_op,

            // -------- capabilities: permission probing --------
            commands::capabilities::probe_account_capabilities,
            commands::capabilities::probe_bucket_capabilities,
            commands::capabilities::get_account_capabilities,
            commands::capabilities::get_bucket_capabilities,

            // -------- logs: diagnostic file access --------
            commands::logs::get_log_dir,
            commands::logs::get_log_tail,

            // -------- request_logs: S3 API call history --------
            commands::request_logs::list_request_logs,
            commands::request_logs::count_request_logs,
            commands::request_logs::clear_request_logs,
            commands::request_logs::purge_old_request_logs,

            // -------- portable: backup / restore / import / export --------
            commands::portable::export_config,
            commands::portable::import_config,
            commands::portable::backup_database,
            commands::portable::stage_restore,
            commands::portable::clear_app_data,

            // -------- encryption: per-bucket age (X25519 + ChaCha20-Poly1305) --------
            commands::encryption::enable_bucket_encryption,
            commands::encryption::disable_bucket_encryption,
            commands::encryption::get_bucket_encryption_status,
            commands::encryption::export_encryption_key,
            commands::encryption::save_encryption_key_export,
            commands::encryption::import_encryption_identity,
            commands::encryption::import_encryption_identity_from_file,
            commands::encryption::has_encryption_identity,
            commands::encryption::list_encrypted_buckets,

            // -------- browse: cache-aware navigation --------
            commands::browse::browse_prefix,

            // -------- night watcher: background local-dir -> S3 sync --------
            commands::night_watcher::nw_list_watches,
            commands::night_watcher::nw_add_watch,
            commands::night_watcher::nw_update_watch,
            commands::night_watcher::nw_delete_watch,
            commands::night_watcher::nw_set_watch_enabled,
            commands::night_watcher::nw_get_status,
            commands::night_watcher::nw_pick_tree,

            // -------- device: native OS/arch/model for bug reports --------
            get_device_info,

            // -------- notifications: native builder with icon + stable id --------
            notify_ex,

            // -------- Android SAF: stream SAF URIs into/out of app cache without loading files into JS --------
            finalize_saf_download,
            delete_saf_document,
            stage_saf_upload,
            set_transfer_service,

            // -------- desktop: guaranteed background-run quit path --------
            #[cfg(not(target_os = "android"))]
            nw_quit_background,

            // -------- mcp: local AI server config (desktop only) --------
            #[cfg(not(target_os = "android"))]
            commands::mcp::mcp_get_config,
            #[cfg(not(target_os = "android"))]
            commands::mcp::mcp_set_config,
            #[cfg(not(target_os = "android"))]
            commands::mcp::mcp_regenerate_token,
            #[cfg(not(target_os = "android"))]
            commands::mcp::mcp_status,

            // -------- dev: debug helpers --------
            #[cfg(debug_assertions)]
            open_devtools,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app_handle, _event| {
            // Desktop close-guard: while a watch is enabled and a tray exists,
            // closing the main window hides it instead of quitting so the
            // Night Watcher loop keeps running. On a no-tray host the guard is
            // disabled (close == real quit) so the user is never trapped.
            #[cfg(not(target_os = "android"))]
            if let tauri::RunEvent::WindowEvent {
                label,
                event: tauri::WindowEvent::CloseRequested { api, .. },
                ..
            } = &_event
            {
                if label == "main"
                    && app_lifecycle::should_background()
                    && !app_lifecycle::quit_requested()
                {
                    if app_lifecycle::tray_available() {
                        // Tray present: hide to keep the loop running.
                        api.prevent_close();
                        if let Some(w) = _app_handle.get_webview_window("main") {
                            let _ = w.hide();
                        }
                    } else {
                        // No tray to restore from: let the close proceed as a
                        // real quit so the user is not trapped. Background sync
                        // stops with the process.
                        tracing::warn!(
                            "closing window quits: no system tray, background sync stops"
                        );
                    }
                }
            }
        });
}
