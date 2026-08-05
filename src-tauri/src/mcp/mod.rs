//! Local MCP server (desktop only).
//!
//! Serves a Streamable HTTP endpoint on `127.0.0.1:<port>/mcp` so a local AI
//! client can drive S3 ops. It shares the one `AppState` with the rest of the
//! app: tools call the same methods the Tauri commands do, so there is a single
//! `TransferManager` and no second SQLite writer.
//!
//! Transport is hand-rolled JSON-RPC 2.0 over HTTP POST (stateless, no session
//! id), which is what current clients speak. Auth lives in [`auth`]: local
//! Origin/Host plus a bearer token.

mod auth;
mod format;
mod tools;

use std::sync::{Arc, Mutex, OnceLock};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::middleware;
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// Keychain key for the MCP bearer token. Not stored in SQLite.
const TOKEN_KEY: &str = "mcp_bearer_token";

/// Per-run tool context. Captured when the server starts, so toggling a setting
/// restarts the listener (see [`apply`]) rather than mutating a live server.
pub struct McpCtx {
    pub state: AppState,
    pub token: String,
    pub allow_write: bool,
    pub allow_delete: bool,
    pub bind_all_accounts: bool,
    pub disabled_tools: Vec<String>,
    pub disabled_accounts: Vec<String>,
    /// Canonicalized folder the upload/download tools are confined to. `None`
    /// (unset, empty, or unresolvable) means file transfers are refused.
    pub fs_root: Option<std::path::PathBuf>,
}

struct Running {
    cancel: CancellationToken,
    port: u16,
}

fn slot() -> &'static Mutex<Option<Running>> {
    static SERVER: OnceLock<Mutex<Option<Running>>> = OnceLock::new();
    SERVER.get_or_init(|| Mutex::new(None))
}

/// Port the server is currently bound to, if running.
pub fn running_port() -> Option<u16> {
    slot().lock().ok().and_then(|g| g.as_ref().map(|r| r.port))
}

/// Stop the running server, if any.
pub fn stop() {
    if let Ok(mut g) = slot().lock() {
        if let Some(r) = g.take() {
            r.cancel.cancel();
        }
    }
}

/// Reconcile the running server with current settings. Stops any live server,
/// then starts a fresh one when `mcp_enabled`. Also reflects enabled state to
/// the desktop background-run gate. Call at startup and after a config change.
pub async fn apply(state: &AppState) -> AppResult<()> {
    let s = state.db.settings_load().await?;
    // set_mcp_enabled mutates the tray and (on macOS) the activation policy,
    // both main-thread only. apply() runs off the main thread from the config
    // commands, so hop over (mirrors nw_refresh_service).
    let app = state.app.clone();
    let enabled = s.mcp_enabled;
    let _ = state.app.run_on_main_thread(move || {
        crate::app_lifecycle::set_mcp_enabled(&app, enabled);
    });
    stop();
    if !s.mcp_enabled {
        return Ok(());
    }
    let token = ensure_token().await?;
    // Canonicalize the sandbox root once, resolving symlinks and `..` so the
    // per-call containment check compares real paths. An unresolvable root
    // (missing dir) collapses to None, which refuses transfers.
    let fs_root = s
        .mcp_fs_root
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .and_then(|p| std::fs::canonicalize(crate::validate::expand_home(p)).ok());
    let ctx = Arc::new(McpCtx {
        state: state.clone(),
        token,
        allow_write: s.mcp_allow_write,
        allow_delete: s.mcp_allow_delete,
        bind_all_accounts: s.mcp_bind_all_accounts,
        disabled_tools: s.mcp_disabled_tools.clone(),
        disabled_accounts: s.mcp_disabled_accounts.clone(),
        fs_root,
    });
    let cancel = CancellationToken::new();
    serve(ctx, s.mcp_port, cancel.clone()).await?;
    if let Ok(mut g) = slot().lock() {
        *g = Some(Running { cancel, port: s.mcp_port });
    }
    Ok(())
}

async fn serve(ctx: Arc<McpCtx>, port: u16, cancel: CancellationToken) -> AppResult<()> {
    let app = Router::new()
        .route("/mcp", post(handle_post).get(handle_get))
        .layer(middleware::from_fn_with_state(ctx.clone(), auth::guard))
        .with_state(ctx);

    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .map_err(|e| AppError::Io(format!("MCP bind on 127.0.0.1:{port} failed: {e}")))?;

    tokio::spawn(async move {
        let shutdown = async move {
            cancel.cancelled().await;
        };
        if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(shutdown).await {
            tracing::warn!("MCP server exited: {e}");
        }
    });
    tracing::info!("MCP server listening on http://127.0.0.1:{port}/mcp");
    Ok(())
}

/// We do not offer the optional server-to-client GET stream.
async fn handle_get() -> StatusCode {
    StatusCode::METHOD_NOT_ALLOWED
}

async fn handle_post(State(ctx): State<Arc<McpCtx>>, body: Bytes) -> Response {
    let value: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return json_response(&rpc_error(Value::Null, -32700, &format!("parse error: {e}")))
        }
    };

    if let Some(arr) = value.as_array() {
        let mut out = Vec::new();
        for m in arr {
            if let Some(r) = process_message(&ctx, m).await {
                out.push(r);
            }
        }
        if out.is_empty() {
            return accepted();
        }
        return json_response(&Value::Array(out));
    }

    match process_message(&ctx, &value).await {
        Some(r) => json_response(&r),
        None => accepted(),
    }
}

/// Handle one JSON-RPC message. Returns `None` for notifications (no `id`).
async fn process_message(ctx: &Arc<McpCtx>, msg: &Value) -> Option<Value> {
    if msg.get("id").is_none() {
        // A notification (e.g. notifications/initialized). Nothing to return.
        return None;
    }
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

    let result: Result<Value, (i64, String)> = match method {
        "initialize" => Ok(initialize_result(msg)),
        "tools/list" => Ok(json!({
            "tools": tools::list(ctx.allow_write, ctx.allow_delete)
                .into_iter()
                .filter(|t| {
                    t.get("name")
                        .and_then(|n| n.as_str())
                        .map(|n| !ctx.disabled_tools.iter().any(|d| d == n))
                        .unwrap_or(true)
                })
                .collect::<Vec<_>>()
        })),
        "tools/call" => {
            let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if name.is_empty() {
                Err((-32602, "missing tool name".into()))
            } else {
                let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                Ok(tools::call(ctx, name, &args).await)
            }
        }
        "ping" => Ok(json!({})),
        other => Err((-32601, format!("method not found: {other}"))),
    };

    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err((code, message)) => rpc_error(id, code, &message),
    })
}

fn initialize_result(msg: &Value) -> Value {
    let pv = msg
        .get("params")
        .and_then(|p| p.get("protocolVersion"))
        .and_then(|v| v.as_str())
        .unwrap_or("2025-11-25")
        .to_string();
    json!({
        "protocolVersion": pv,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "cosmog", "version": env!("CARGO_PKG_VERSION") }
    })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn json_response(body: &Value) -> Response {
    let text = serde_json::to_string(body).unwrap_or_else(|_| "{}".into());
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(text.into())
        .unwrap_or_else(|_| Response::new(String::new().into()))
}

fn accepted() -> Response {
    Response::builder()
        .status(StatusCode::ACCEPTED)
        .body(String::new().into())
        .unwrap_or_else(|_| Response::new(String::new().into()))
}

// ---- bearer token (keychain) ----

/// Load the bearer token, minting one on first use. Runs on a blocking thread
/// because keyring access is synchronous.
pub async fn ensure_token() -> AppResult<String> {
    tokio::task::spawn_blocking(load_or_create_token)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
}

/// Mint a fresh token, replacing any existing one.
pub async fn regenerate_token() -> AppResult<String> {
    tokio::task::spawn_blocking(|| {
        let t = gen_token();
        crate::secrets::set_secret(TOKEN_KEY, &t)?;
        Ok(t)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
}

fn load_or_create_token() -> AppResult<String> {
    match crate::secrets::get_secret(TOKEN_KEY) {
        Ok(t) => Ok(t),
        Err(AppError::NotFound(_)) => {
            let t = gen_token();
            crate::secrets::set_secret(TOKEN_KEY, &t)?;
            Ok(t)
        }
        Err(e) => Err(e),
    }
}

fn gen_token() -> String {
    use rand::Rng;
    let mut b = [0u8; 32];
    rand::thread_rng().fill(&mut b[..]);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// One advertised tool: name plus its human description. Used by the settings
/// UI to show what the AI can do right now.
#[derive(serde::Serialize)]
pub struct AdvertisedTool {
    pub name: String,
    pub description: String,
    /// False when the user turned this tool off individually.
    pub enabled: bool,
}

/// Candidate tools for the given write/delete gates, each carrying whether it is
/// enabled after the per-tool disable set is applied. The UI lists all of them
/// so a disabled tool can be turned back on.
pub fn advertised_tools(
    allow_write: bool,
    allow_delete: bool,
    disabled: &[String],
) -> Vec<AdvertisedTool> {
    tools::list(allow_write, allow_delete)
        .iter()
        .filter_map(|t| {
            let name = t.get("name").and_then(|n| n.as_str())?;
            let desc = t.get("description").and_then(|d| d.as_str()).unwrap_or("");
            Some(AdvertisedTool {
                name: name.to_string(),
                description: desc.to_string(),
                enabled: !disabled.iter().any(|d| d == name),
            })
        })
        .collect()
}
