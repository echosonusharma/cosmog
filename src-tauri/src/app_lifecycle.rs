//! Desktop background-run lifecycle: the process survives window close only while a watch or MCP
//! is enabled ([`apply`] arms tray/autostart/macOS policy; no tray host disables the close-guard).

#![cfg(not(target_os = "android"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Manager};

static HAS_ENABLED_WATCH: AtomicBool = AtomicBool::new(false);
static MCP_ENABLED: AtomicBool = AtomicBool::new(false);
/// Set by explicit quit (tray Quit / FE quit command); makes the close-guard stand down.
static QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);
static TRAY_AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Held so the TrayIcon is never dropped (dropping removes the tray); built once, reused.
static TRAY: Mutex<Option<TrayIcon>> = Mutex::new(None);

pub fn has_enabled_watch() -> bool {
    HAS_ENABLED_WATCH.load(Ordering::SeqCst)
}

/// True when a watch or the MCP server wants the process kept alive; the close-guard reads this.
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

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Single choke point: arms/disarms background running. Every OS call is best-effort — failures
/// log a warning and never panic.
pub fn apply(app: &AppHandle, enabled: bool) {
    HAS_ENABLED_WATCH.store(enabled, Ordering::SeqCst);
    refresh(app);
}

pub fn set_mcp_enabled(app: &AppHandle, enabled: bool) {
    MCP_ENABLED.store(enabled, Ordering::SeqCst);
    refresh(app);
}

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

fn ensure_tray(app: &AppHandle) {
    let mut guard = TRAY.lock().expect("tray mutex poisoned");
    if guard.is_some() {
        TRAY_AVAILABLE.store(true, Ordering::SeqCst);
        return;
    }
    match build_tray(app) {
        Ok(tray) => {
            *guard = Some(tray);
            TRAY_AVAILABLE.store(true, Ordering::SeqCst);
        }
        Err(e) => {
            // No tray host (common on Wayland/Hyprland): run without a tray; the close-guard
            // stays disabled so closing the window really quits.
            tracing::warn!("tray build failed, running without tray: {e}");
            TRAY_AVAILABLE.store(false, Ordering::SeqCst);
        }
    }
}

/// Dropping the held TrayIcon is what removes it from the OS tray.
fn remove_tray() {
    let mut guard = TRAY.lock().expect("tray mutex poisoned");
    *guard = None;
    TRAY_AVAILABLE.store(false, Ordering::SeqCst);
}

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
