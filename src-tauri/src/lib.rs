//! Cosmog backend — desktop client for S3-compatible object stores. Layering: `commands/*` ->
//! AppState/TransferManager (persistent transfer queue) -> [`store::ObjectStore`] (S3) -> SQLite + OS keyring.
//! New provider: `store/<name>.rs` impl + [`providers::Protocol`] variant. Schema: append-only migrations in `db/mod.rs`.

// Modules pub for the integration-tests crate; only `commands::*` serves as the Tauri API surface.
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

/// Native notification: a stable `id` REPLACES the prior one; builder allows an Android drawable icon.
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

/// Streams a finished download from cache into a SAF content:// URI (chunked, multi-GB safe).
#[tauri::command]
async fn finalize_saf_download(cache_path: String, uri: String) -> Result<u64, String> {
    crate::saf::finalize_saf_download(cache_path, uri).await
}

/// Removes the SAF placeholder document after a canceled/failed download (no 0-byte leftovers).
#[tauri::command]
async fn delete_saf_document(uri: String) -> Result<bool, String> {
    crate::saf::delete_saf_document(uri).await
}

/// Stages a SAF `content://` URI into app cache; returns a usable fs path plus the display name for the S3 key.
#[tauri::command]
async fn stage_saf_upload(
    uri: String,
    dest_dir: String,
) -> Result<crate::saf::SafStagedUpload, String> {
    crate::saf::stage_saf_upload(uri, dest_dir).await
}

/// Toggles the Android foreground TransferService (FE polling; no-op off Android).
#[tauri::command]
fn set_transfer_service(active: bool) -> Result<(), String> {
    crate::saf::set_transfer_service(active)
}

/// Real OS/arch/model info for bug reports, resolved natively (not the WebView navigator string).
#[tauri::command]
fn get_device_info() -> Result<crate::device::DeviceInfo, String> {
    crate::device::get_device_info()
}

/// Guaranteed quit for no-tray hosts: marks quit requested (close-guard stands down), then exits.
#[cfg(not(target_os = "android"))]
#[tauri::command]
fn nw_quit_background(app: tauri::AppHandle) {
    app_lifecycle::request_quit();
    app.exit(0);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();

    // Desktop-only plugins. single-instance must register FIRST so a second launch is routed
    // to the running instance before any other plugin initializes; autostart launches with --hidden.
    #[cfg(not(target_os = "android"))]
    let builder = builder
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_window_state::Builder::default().build());

    builder
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .setup(|app| {
            use tracing_subscriber::layer::SubscriberExt;
            use tracing_subscriber::util::SubscriberInitExt;
            use tracing_subscriber::Layer;

            let app_dir = app.path().app_data_dir().expect("resolve app data dir");

            // Apply a requested full wipe (marker from clear_app_data) BEFORE anything opens
            // files under app_dir, so no open handles block removal.
            let wipe_marker = app_dir.join("pending_wipe");
            if wipe_marker.exists() {
                match std::fs::remove_dir_all(&app_dir) {
                    Ok(()) => {
                        let _ = std::fs::create_dir_all(&app_dir);
                    }
                    Err(e) => {
                        // Drop the marker so a failed wipe isn't retried every boot;
                        // the user can retry via Settings.
                        eprintln!("pending_wipe: remove_dir_all failed: {e}");
                        let _ = std::fs::remove_file(&wipe_marker);
                    }
                }
            }

            let db_path = app_dir.join("cosmog.sqlite");
            let log_dir = app_dir.join("logs");

            std::fs::create_dir_all(&log_dir).ok();
            let file_appender = tracing_appender::rolling::Builder::new()
                .rotation(tracing_appender::rolling::Rotation::DAILY)
                .filename_prefix("cosmog.log")
                .max_log_files(30)
                .build(&log_dir)
                .unwrap_or_else(|_| tracing_appender::rolling::daily(&log_dir, "cosmog.log"));
            let (file_writer, guard) = tracing_appender::non_blocking(file_appender);
            // The guard must outlive the process or buffered logs are lost at shutdown.
            static LOG_GUARD: std::sync::OnceLock<tracing_appender::non_blocking::WorkerGuard> =
                std::sync::OnceLock::new();
            let _ = LOG_GUARD.set(guard);

            let env_filter = || {
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
            };

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
                // Apply a staged restore BEFORE Db::open, re-validating right before the swap
                // (magic bytes + PRAGMA quick_check): a corrupt file with a valid header would brick all accounts.
                let pending = db_path.with_extension("restore_pending");
                if pending.exists() {
                    let magic_ok = match tokio::fs::File::open(&pending).await {
                        Ok(mut f) => {
                            use tokio::io::AsyncReadExt;
                            let mut header = [0u8; 16];
                            f.read_exact(&mut header).await.is_ok()
                                && &header == b"SQLite format 3\0"
                        }
                        Err(_) => false,
                    };
                    let valid = magic_ok && {
                        let check_path = pending.clone();
                        tokio::task::spawn_blocking(move || -> bool {
                            (|| -> rusqlite::Result<bool> {
                                let conn = rusqlite::Connection::open_with_flags(
                                    &check_path,
                                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                                )?;
                                let status: String =
                                    conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
                                Ok(status.eq_ignore_ascii_case("ok"))
                            })()
                            .unwrap_or(false)
                        })
                        .await
                        .unwrap_or(false)
                    };
                    if !valid {
                        tracing::warn!(
                            "restore_pending at {} is not a healthy SQLite DB; ignoring + removing",
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
                // Reap transfers orphaned by a crash so the UI shows no ghost-running rows.
                // Desktop only: on Android the sibling :nightwatch process may own live rows.
                #[cfg(not(target_os = "android"))]
                if let Err(e) = db.reap_orphan_transfers().await {
                    tracing::warn!("reap_orphan_transfers failed: {e}");
                }
                // Android: main process owns `user` rows, :nightwatch owns `nightwatch` ones;
                // reap only our orphans so a swipe-killed upload becomes a retryable Failed row.
                #[cfg(target_os = "android")]
                if let Err(e) = db.reap_orphan_transfers_by_origin("user").await {
                    tracing::warn!("reap_orphan_transfers_by_origin failed: {e}");
                }
                // Concurrency changes take effect next launch: the Semaphore isn't resizable in place.
                let settings = db.settings_load().await.unwrap_or_default();
                // Apply proxy / custom-CA env BEFORE any SDK client is built.
                crate::db::settings::apply_network_env(&settings);
                let concurrency = settings.transfer_concurrency as usize;

                // Critical path ends here: manage state + show the window before slow
                // background work (log pruning, enc_tmp sweep, scheduler/tray/MCP startup).
                let state = AppState::new(
                    db,
                    concurrency,
                    log_dir,
                    db_path.clone(),
                    handle.clone(),
                );
                handle.manage(state.clone());

                // Autostart passes --hidden; config has visible:false, so skipping show keeps it hidden.
                #[cfg(not(target_os = "android"))]
                if !std::env::args().any(|a| a == "--hidden") {
                    if let Some(w) = handle.get_webview_window("main") {
                        let _ = w.show();
                    }
                }

                let bg_handle = handle.clone();
                let ttl_days = settings.request_log_ttl_days as i64;
                tauri::async_runtime::spawn(async move {
                    let ttl_cutoff = chrono::Utc::now().timestamp() - (ttl_days * 86_400);
                    if let Err(e) = state.db.delete_old_request_logs(ttl_cutoff).await {
                        tracing::warn!("request log TTL cleanup failed: {e}");
                    }
                    // Crash leftovers under <db_dir>/enc_tmp/ are ciphertext-only, safe to delete
                    // unconditionally (staged for uploads that never completed).
                    if let Some(parent) = db_path.parent() {
                        let enc_tmp = parent.join("enc_tmp");
                        if enc_tmp.exists() {
                            let _ = tokio::task::spawn_blocking(move || {
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
                            })
                            .await;
                        }
                    }
                    // Cancel token parked in a OnceLock so it lives until process exit.
                    static SCHEDULER_CANCEL: std::sync::OnceLock<
                        tokio_util::sync::CancellationToken,
                    > = std::sync::OnceLock::new();
                    let _ = SCHEDULER_CANCEL.set(scheduler::spawn(state.clone()));
                    // Night Watcher: bg local-dir -> S3 sync; token parked for process lifetime. On Android the
                    // loop runs in the :nightwatch service process — a duplicate here would double-scan/race nw_file_state.
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
                        app_lifecycle::apply(&bg_handle, enabled);
                    }
                    // MCP server starts only when enabled in settings; its flag feeds the background-run gate.
                    #[cfg(not(target_os = "android"))]
                    if let Err(e) = mcp::apply(&state).await {
                        tracing::warn!("MCP server start failed: {e}");
                    }
                    commands::night_watcher::nw_refresh_service(&state).await;
                    let _ = &bg_handle;
                });
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::accounts::add_account,
            commands::accounts::list_accounts,
            commands::accounts::get_account,
            commands::accounts::update_account,
            commands::accounts::delete_account,
            commands::accounts::test_account,
            commands::accounts::detect_account_region,

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

            commands::transfers::enqueue_upload,
            commands::transfers::enqueue_download,
            commands::transfers::list_transfers,
            commands::transfers::get_transfer,
            commands::transfers::cancel_transfer,
            commands::transfers::retry_transfer,
            commands::transfers::clear_completed_transfers,
            commands::transfers::clear_transfer,

            commands::search::search_objects,
            commands::search::sync_prefix,
            commands::search::bucket_index_status,
            commands::search::enable_bucket_index,
            commands::search::cancel_bucket_scan,
            commands::search::reindex_bucket,
            commands::search::disable_bucket_index,
            commands::search::bucket_stats,
            commands::search::set_bucket_auto_reindex,

            commands::settings::get_settings,
            commands::settings::update_settings,
            commands::settings::reset_settings,

            commands::bulk::delete_folder_cmd,
            commands::bulk::upload_directory_cmd,
            commands::bulk::download_directory_cmd,
            commands::bulk::cancel_bulk_op,

            commands::capabilities::probe_account_capabilities,
            commands::capabilities::probe_bucket_capabilities,
            commands::capabilities::get_account_capabilities,
            commands::capabilities::get_bucket_capabilities,

            commands::logs::get_log_dir,
            commands::logs::get_log_tail,

            commands::request_logs::list_request_logs,
            commands::request_logs::count_request_logs,
            commands::request_logs::get_request_log_stats,
            commands::request_logs::clear_request_logs,
            commands::request_logs::purge_old_request_logs,

            commands::portable::export_config,
            commands::portable::import_config,
            commands::portable::backup_database,
            commands::portable::stage_restore,
            commands::portable::clear_app_data,

            commands::encryption::enable_bucket_encryption,
            commands::encryption::disable_bucket_encryption,
            commands::encryption::get_bucket_encryption_status,
            commands::encryption::export_encryption_key,
            commands::encryption::save_encryption_key_export,
            commands::encryption::import_encryption_identity,
            commands::encryption::import_encryption_identity_from_file,
            commands::encryption::has_encryption_identity,
            commands::encryption::list_encrypted_buckets,

            commands::browse::browse_prefix,

            commands::night_watcher::nw_list_watches,
            commands::night_watcher::nw_add_watch,
            commands::night_watcher::nw_update_watch,
            commands::night_watcher::nw_delete_watch,
            commands::night_watcher::nw_set_watch_enabled,
            commands::night_watcher::nw_get_status,
            commands::night_watcher::nw_pick_tree,

            get_device_info,

            notify_ex,

            finalize_saf_download,
            delete_saf_document,
            stage_saf_upload,
            set_transfer_service,

            #[cfg(not(target_os = "android"))]
            nw_quit_background,

            #[cfg(not(target_os = "android"))]
            commands::mcp::mcp_get_config,
            #[cfg(not(target_os = "android"))]
            commands::mcp::mcp_set_config,
            #[cfg(not(target_os = "android"))]
            commands::mcp::mcp_regenerate_token,
            #[cfg(not(target_os = "android"))]
            commands::mcp::mcp_status,

            #[cfg(debug_assertions)]
            open_devtools,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app_handle, _event| {
            // Close-guard: while background running is armed (and quit not requested), hide the
            // main window instead of quitting so Night Watcher survives. No tray => close really quits.
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
                        api.prevent_close();
                        if let Some(w) = _app_handle.get_webview_window("main") {
                            let _ = w.hide();
                        }
                    } else {
                        tracing::warn!(
                            "closing window quits: no system tray, background sync stops"
                        );
                    }
                }
            }
        });
}
