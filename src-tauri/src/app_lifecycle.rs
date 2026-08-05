//! Desktop-only background-run lifecycle for Night Watcher.
//!
//! The process hosts the always-running Night Watcher tokio loop. On desktop
//! we want the process to survive the window being closed, but ONLY while at
//! least one watch is enabled. Zero enabled watches means closing the window
//! fully exits.
//!
//! [`apply`] is the single choke point: it arms or disarms background running
//! by wiring a tray icon, OS autostart, and (on macOS) the activation policy.
//!
//! No-tray fallback: on some Linux setups (Wayland/Hyprland without a
//! StatusNotifier host) a tray cannot be created. In that case we set
//! [`tray_available`] to false and the close-guard in `lib.rs` is disabled so
//! the user is never trapped: closing the window really quits.

#![cfg(not(target_os = "android"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Manager};

/// True while at least one watch is enabled.
static HAS_ENABLED_WATCH: AtomicBool = AtomicBool::new(false);
/// True while the MCP server is enabled. Either signal keeps the process
/// backgrounded so the window can close without stopping sync or MCP.
static MCP_ENABLED: AtomicBool = AtomicBool::new(false);
/// Set once the user explicitly asks to quit (tray Quit or the FE quit
/// command) so the close-guard lets the window close for real.
static QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);
/// False when the tray could not be created (no StatusNotifier host). When
/// false the close-guard is disabled so close == real quit.
static TRAY_AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Holds the live tray icon so it is not dropped (dropping removes the tray).
/// Built once, reused across `apply` calls.
static TRAY: Mutex<Option<TrayIcon>> = Mutex::new(None);

pub fn has_enabled_watch() -> bool {
    HAS_ENABLED_WATCH.load(Ordering::SeqCst)
}

/// True when anything wants the process kept alive in the background: an
/// enabled watch or the MCP server. The close-guard reads this.
pub fn should_background() -> bool {
    HAS_ENABLED_WATCH.load(Ordering::SeqCst) || MCP_ENABLED.load(Ordering::SeqCst)
}

pub fn quit_requested() -> bool {
    QUIT_REQUESTED.load(Ordering::SeqCst)
}

pub fn request_quit() {
    QUIT_REQUESTED.store(true, Ordering::SeqCst);
}

pub fn tray_available() -> bool {
    TRAY_AVAILABLE.load(Ordering::SeqCst)
}

/// Show and focus the main window. Best-effort.
fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// The single choke point. Arms background running when `enabled`, disarms
/// otherwise. Every OS call is best-effort: a failure logs a warning and never
/// panics, so a missing tray or autostart backend cannot crash the app.
pub fn apply(app: &AppHandle, enabled: bool) {
    HAS_ENABLED_WATCH.store(enabled, Ordering::SeqCst);
    refresh(app);
}

/// Set the MCP-enabled signal and re-evaluate background running.
pub fn set_mcp_enabled(app: &AppHandle, enabled: bool) {
    MCP_ENABLED.store(enabled, Ordering::SeqCst);
    refresh(app);
}

/// Arm or disarm background running based on the combined signals.
fn refresh(app: &AppHandle) {
    if should_background() {
        ensure_tray(app);
        set_autostart(app, true);
        #[cfg(target_os = "macos")]
        set_activation_policy(app, tauri::ActivationPolicy::Accessory);
    } else {
        remove_tray();
        set_autostart(app, false);
        #[cfg(target_os = "macos")]
        set_activation_policy(app, tauri::ActivationPolicy::Regular);
    }
}

/// Build the tray once and store it. On a host without a StatusNotifier the
/// build fails; we log and leave TRAY_AVAILABLE=false.
fn ensure_tray(app: &AppHandle) {
    let mut guard = TRAY.lock().expect("tray mutex poisoned");
    if guard.is_some() {
        // Already built and held. Nothing to rebuild.
        TRAY_AVAILABLE.store(true, Ordering::SeqCst);
        return;
    }
    match build_tray(app) {
        Ok(tray) => {
            *guard = Some(tray);
            TRAY_AVAILABLE.store(true, Ordering::SeqCst);
        }
        Err(e) => {
            // No tray host (common on Wayland/Hyprland). Keep running without
            // a tray; the close-guard stays disabled so the user can still
            // quit by closing the window and relies on autostart + the FE
            // quit button.
            tracing::warn!("tray build failed, running without tray: {e}");
            TRAY_AVAILABLE.store(false, Ordering::SeqCst);
        }
    }
}

/// Drop the held tray icon, which removes it from the OS tray.
fn remove_tray() {
    let mut guard = TRAY.lock().expect("tray mutex poisoned");
    // Dropping the TrayIcon removes it from the system tray.
    *guard = None;
    TRAY_AVAILABLE.store(false, Ordering::SeqCst);
}

/// Enable or disable OS autostart. Best-effort.
fn set_autostart(app: &AppHandle, enable: bool) {
    use tauri_plugin_autostart::ManagerExt;
    let manager = app.autolaunch();
    let res = if enable {
        manager.enable()
    } else {
        manager.disable()
    };
    if let Err(e) = res {
        tracing::warn!("autostart {} failed: {e}", if enable { "enable" } else { "disable" });
    }
}

#[cfg(target_os = "macos")]
fn set_activation_policy(app: &AppHandle, policy: tauri::ActivationPolicy) {
    if let Err(e) = app.set_activation_policy(policy) {
        tracing::warn!("set_activation_policy failed: {e}");
    }
}

/// Build the tray icon with an "Open Cosmog" / "Quit" menu.
fn build_tray(app: &AppHandle) -> tauri::Result<TrayIcon> {
    let open_item = MenuItem::with_id(app, "open", "Open Cosmog", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open_item, &quit_item])?;

    let mut builder = TrayIconBuilder::new()
        .tooltip("Cosmog - background sync active")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "quit" => {
                request_quit();
                app.exit(0);
            }
            _ => {}
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }

    builder.build(app)
}
