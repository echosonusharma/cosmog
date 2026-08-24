//! Android headless Night Watcher host in the dedicated `:nightwatch` service process, which
//! survives wry's exit-on-Activity-destroy; opens the same DB via `context.getDataDir()`.

#![cfg(target_os = "android")]

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use dashmap::{DashMap, DashSet};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::db::settings::apply_network_env;
use crate::db::Db;
use crate::error::AppResult;
use crate::night_watcher::{run_loop, NwCtx};
use crate::providers::build_store;
use crate::store::region_retry::RegionRetryStore;
use crate::store::ObjectStore;
use crate::transfer::TransferManager;

/// Lightweight [`NwCtx`] for the service process: real `Db` + `TransferManager` + store cache,
/// minus the `AppHandle` and request-log layer (no webview).
#[derive(Clone)]
struct NwHeadlessCtx {
    db: Db,
    db_path: PathBuf,
    transfers: TransferManager,
    clients: Arc<DashMap<String, Arc<dyn ObjectStore>>>,
    inflight: Arc<DashSet<(String, String)>>,
    scan_inflight: Arc<DashSet<String>>,
}

impl NwCtx for NwHeadlessCtx {
    fn db(&self) -> &Db {
        &self.db
    }
    fn db_path(&self) -> &Path {
        &self.db_path
    }
    fn transfers(&self) -> &TransferManager {
        &self.transfers
    }
    async fn store_for(&self, account_id: &str) -> AppResult<Arc<dyn ObjectStore>> {
        if let Some(existing) = self.clients.get(account_id) {
            return Ok(existing.clone());
        }
        let account = self.db.get_account(account_id).await?;
        let mut inner = build_store(&account).await?;
        // Real AWS only: route/retry per bucket so cross-region buckets don't PermanentRedirect.
        if account.endpoint.is_none() {
            inner = Arc::new(RegionRetryStore::new(inner, account.clone()));
        }
        Ok(self
            .clients
            .entry(account_id.to_string())
            .or_insert(inner)
            .clone())
    }
    fn nw_claim(&self, watch_id: &str, rel_path: &str) -> bool {
        self.inflight
            .insert((watch_id.to_string(), rel_path.to_string()))
    }
    fn nw_unclaim(&self, watch_id: &str, rel_path: &str) {
        self.inflight
            .remove(&(watch_id.to_string(), rel_path.to_string()));
    }
    fn nw_scan_claim(&self, watch_id: &str) -> bool {
        self.scan_inflight.insert(watch_id.to_string())
    }
    fn nw_scan_unclaim(&self, watch_id: &str) {
        self.scan_inflight.remove(watch_id);
    }
}

// Process-lifetime singletons: runtime + ctx are built once and REUSED across service
// stop/restart, so a restart never spawns rival Db/TransferManagers racing over the same rows.
static NW_RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
static NW_CTX: std::sync::OnceLock<NwHeadlessCtx> = std::sync::OnceLock::new();
static NW_CANCEL: std::sync::Mutex<Option<CancellationToken>> = std::sync::Mutex::new(None);
// True while a loop is live; a second onCreate is a no-op.
static NW_RUNNING: AtomicBool = AtomicBool::new(false);
static LOG_INIT: std::sync::Once = std::sync::Once::new();

/// JNI `context.getDataDir()`: same path Tauri's `app_data_dir()` resolves, so the service
/// opens the DB file the main process wrote.
fn resolve_data_dir() -> Result<PathBuf, String> {
    use jni::objects::{JObject, JString};
    use jni::JavaVM;

    let ctx = ndk_context::android_context();
    if ctx.vm().is_null() || ctx.context().is_null() {
        return Err("android context not initialized".into());
    }
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| format!("JavaVM: {e}"))?;
    let mut env = vm
        .attach_current_thread()
        .map_err(|e| format!("attach: {e}"))?;
    let context = unsafe { JObject::from_raw(ctx.context().cast()) };
    let file = env
        .call_method(&context, "getDataDir", "()Ljava/io/File;", &[])
        .map_err(|e| format!("getDataDir: {e}"))?
        .l()
        .map_err(|e| format!("getDataDir.l: {e}"))?;
    let s = env
        .call_method(&file, "getAbsolutePath", "()Ljava/lang/String;", &[])
        .map_err(|e| format!("getAbsolutePath: {e}"))?
        .l()
        .map_err(|e| format!("getAbsolutePath.l: {e}"))?;
    let path: String = env
        .get_string(&JString::from(s))
        .map_err(|e| format!("get_string: {e}"))?
        .into();
    Ok(PathBuf::from(path))
}

/// Service-process logging to stdout (logcat) + a private rolling file, away from the main log writer.
fn init_logging(log_dir: &Path) {
    LOG_INIT.call_once(|| {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        use tracing_subscriber::Layer;

        let _ = std::fs::create_dir_all(log_dir);
        let file_appender = tracing_appender::rolling::daily(log_dir, "cosmog-nw.log");
        let (writer, guard) = tracing_appender::non_blocking(file_appender);
        static LOG_GUARD: std::sync::OnceLock<tracing_appender::non_blocking::WorkerGuard> =
            std::sync::OnceLock::new();
        let _ = LOG_GUARD.set(guard);

        let filter = || {
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
        };
        let stdout_layer = tracing_subscriber::fmt::layer().with_filter(filter());
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_ansi(false)
            .with_filter(filter());
        let _ = tracing_subscriber::registry()
            .with(stdout_layer)
            .with(file_layer)
            .try_init();
    });
}

async fn build_ctx(db_path: PathBuf) -> AppResult<NwHeadlessCtx> {
    let db = Db::open(&db_path).await?;
    // Reap our orphans: a swipe/LMK kill strands `nightwatch` rows (in-memory workers, nothing
    // re-picks). Safe here: sole owner of this origin, nothing live yet; main reaps only `user`.
    match db.reap_orphan_transfers_by_origin("nightwatch").await {
        Ok(n) if n > 0 => info!("reaped {n} orphan nightwatch transfer(s)"),
        Ok(_) => {}
        Err(e) => warn!("reap nightwatch orphans failed: {e}"),
    }
    // Sweep leftover SAF staging from a killed run: nw_stage/ holds per-upload scratch copies,
    // safe to delete unconditionally (mirrors the desktop enc_tmp sweep).
    sweep_nw_stage(&db_path);
    let settings = db.settings_load().await.unwrap_or_default();
    // Same proxy / custom-CA env the main process applies at boot.
    apply_network_env(&settings);
    let transfers = TransferManager::new(db.clone(), settings.transfer_concurrency as usize);
    Ok(NwHeadlessCtx {
        db,
        db_path,
        transfers,
        clients: Arc::new(DashMap::new()),
        inflight: Arc::new(DashSet::new()),
        scan_inflight: Arc::new(DashSet::new()),
    })
}

/// Delete the whole `nw_stage/` scratch tree; clean runs remove the per-upload `<uuid>/`
/// subdirs themselves, so leftovers were orphaned by a killed process. Best-effort, blocking.
fn sweep_nw_stage(db_path: &Path) {
    let Some(stage) = db_path.parent().map(|p| p.join("nw_stage")) else {
        return;
    };
    if !stage.exists() {
        return;
    }
    match std::fs::remove_dir_all(&stage) {
        Ok(()) => info!("swept stale nw_stage dir {}", stage.display()),
        Err(e) => warn!("nw_stage sweep failed: {e}"),
    }
}

/// Fallible core of `startNwSync`, isolated so the JNI shim can `catch_unwind`.
fn start_inner() {
    if NW_RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let data_dir = match resolve_data_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("startNwSync: resolve_data_dir failed: {e}");
            NW_RUNNING.store(false, Ordering::SeqCst);
            return;
        }
    };
    init_logging(&data_dir.join("logs"));

    if NW_RT.get().is_none() {
        match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => {
                let _ = NW_RT.set(rt);
            }
            Err(e) => {
                eprintln!("startNwSync: build runtime failed: {e}");
                NW_RUNNING.store(false, Ordering::SeqCst);
                return;
            }
        }
    }
    let rt = NW_RT.get().expect("runtime set above");

    if NW_CTX.get().is_none() {
        match rt.block_on(build_ctx(data_dir.join("cosmog.sqlite"))) {
            Ok(c) => {
                let _ = NW_CTX.set(c);
            }
            Err(e) => {
                warn!("startNwSync: build_ctx failed: {e}");
                NW_RUNNING.store(false, Ordering::SeqCst);
                return;
            }
        }
    }
    let ctx = NW_CTX.get().expect("ctx set above").clone();

    let token = CancellationToken::new();
    *NW_CANCEL.lock().unwrap_or_else(|p| p.into_inner()) = Some(token.clone());
    // Wakelock acquire is bounded (10 min) so a crash can't leak it, but a longer sync would
    // lose the CPU past the cap. Ping the Java side well inside the window to re-arm it.
    rt.spawn(wakelock_heartbeat(token.clone()));
    rt.spawn(run_loop(ctx, token));
    info!("headless night watcher started (service process)");
}

/// Re-arm the service wakelock every 5 min (Java cap 10 min, wide margin); the loop's
/// cancellation token stops pings the moment the service is torn down.
async fn wakelock_heartbeat(token: CancellationToken) {
    const HEARTBEAT_SECS: u64 = 5 * 60;
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(HEARTBEAT_SECS));
    loop {
        tokio::select! {
            _ = token.cancelled() => break,
            _ = tick.tick() => {
                if let Err(e) = crate::saf::nw_wakelock_heartbeat() {
                    warn!("wakelock heartbeat failed: {e}");
                }
            }
        }
    }
}

fn stop_inner() {
    if let Some(token) = NW_CANCEL
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
    {
        token.cancel();
    }
    NW_RUNNING.store(false, Ordering::SeqCst);
    info!("headless night watcher stop requested");
}

/// Start the headless loop. Idempotent; panics are contained — unwinding across JNI is UB.
#[no_mangle]
pub extern "system" fn Java_com_sonus_cosmog_NightWatchService_startNwSync(
    _env: jni::JNIEnv,
    _this: jni::objects::JObject,
) {
    if std::panic::catch_unwind(AssertUnwindSafe(start_inner)).is_err() {
        eprintln!("startNwSync: panicked (contained)");
        NW_RUNNING.store(false, Ordering::SeqCst);
    }
}

/// Stop the loop (service destroyed); the parked runtime + ctx stay for same-process restart.
#[no_mangle]
pub extern "system" fn Java_com_sonus_cosmog_NightWatchService_stopNwSync(
    _env: jni::JNIEnv,
    _this: jni::objects::JObject,
) {
    if std::panic::catch_unwind(AssertUnwindSafe(stop_inner)).is_err() {
        eprintln!("stopNwSync: panicked (contained)");
    }
}
