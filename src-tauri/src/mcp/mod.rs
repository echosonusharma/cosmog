//! Local MCP server (desktop only): stateless JSON-RPC 2.0 over HTTP POST on
//! `127.0.0.1:<port>/mcp`, dual-era (2026-07-28 + legacy handshake), sharing the app's `AppState`.

mod auth;
mod format;
mod tools;

use std::sync::{Arc, Mutex, OnceLock};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
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

/// All protocol versions spoken, newest first; reported by `server/discover`.
const SUPPORTED_VERSIONS: &[&str] = &["2026-07-28", "2025-11-25", "2025-06-18", "2025-03-26"];
/// Legacy handshake versions (the modern era has no `initialize`).
const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];
const LATEST_LEGACY: &str = "2025-11-25";

// Reserved `_meta` keys carrying per-request protocol metadata (2026-07-28).
const META_PV: &str = "io.modelcontextprotocol/protocolVersion";
const META_CAPS: &str = "io.modelcontextprotocol/clientCapabilities";
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

// JSON-RPC error codes: standard plus the MCP-reserved sub-range.
const E_METHOD_NOT_FOUND: i64 = -32601;
const E_INVALID_PARAMS: i64 = -32602;
const E_HEADER_MISMATCH: i64 = -32020;
const E_UNSUPPORTED_VERSION: i64 = -32022;

/// Per-run tool context captured at server start; toggling a setting restarts
/// the listener ([`apply`]) rather than mutating a live server.
pub struct McpCtx {
    pub state: AppState,
    pub token: String,
    pub allow_write: bool,
    pub allow_delete: bool,
    pub bind_all_accounts: bool,
    pub disabled_tools: Vec<String>,
    pub disabled_accounts: Vec<String>,
    /// Canonicalized transfer-confinement folder; `None` refuses file transfers.
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

pub fn running_port() -> Option<u16> {
    slot().lock().ok().and_then(|g| g.as_ref().map(|r| r.port))
}

pub fn stop() {
    if let Ok(mut g) = slot().lock() {
        if let Some(r) = g.take() {
            r.cancel.cancel();
        }
    }
}

/// Reconcile the running server with current settings: stops any live server,
/// restarts when enabled, and reflects state to the background-run gate.
pub async fn apply(state: &AppState) -> AppResult<()> {
    let s = state.db.settings_load().await?;
    // set_mcp_enabled mutates tray/activation policy, main-thread only on
    // macOS; hop over since apply() runs off-main (mirrors nw_refresh_service).
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
    // Resolve symlinks/`..` once so containment checks compare real paths;
    // an unresolvable root becomes None, refusing transfers.
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
        .route("/mcp", post(handle_post).get(handle_405).delete(handle_405))
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

async fn handle_405() -> StatusCode {
    StatusCode::METHOD_NOT_ALLOWED
}

async fn handle_post(State(ctx): State<Arc<McpCtx>>, headers: HeaderMap, body: Bytes) -> Response {
    let value: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return json_response(&rpc_error(Value::Null, -32700, &format!("parse error: {e}")))
        }
    };

    // Single JSON-RPC message only: batching removed in the 2025-06-18 transport.
    match process_message(&ctx, &headers, &value).await {
        Some((status, r)) => json_status(status, &r),
        None => accepted(),
    }
}

/// Routes to the modern or legacy handler; `None` (notification/response, no
/// `id`) becomes a bare 202.
async fn process_message(
    ctx: &Arc<McpCtx>,
    headers: &HeaderMap,
    msg: &Value,
) -> Option<(StatusCode, Value)> {
    if msg.get("id").is_none() {
        return None;
    }
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

    if method == "server/discover" {
        return Some(discover_result(id));
    }

    // Requests carrying per-request protocolVersion in params._meta follow
    // 2026-07-28 rules; anything else is the legacy initialize era.
    let body_pv = msg
        .get("params")
        .and_then(|p| p.get("_meta"))
        .and_then(|m| m.get(META_PV))
        .and_then(|v| v.as_str());

    Some(match body_pv {
        Some(pv) => modern(ctx, headers, msg, id, method, pv).await,
        None => legacy(ctx, msg, headers, id, method).await,
    })
}

/// Modern (2026-07-28) request: header/body agreement + required metadata,
/// with specced HTTP status + code errors.
async fn modern(
    ctx: &Arc<McpCtx>,
    headers: &HeaderMap,
    msg: &Value,
    id: Value,
    method: &str,
    body_pv: &str,
) -> (StatusCode, Value) {
    // MCP-Protocol-Version / Mcp-Method MUST mirror the body values; a
    // missing/mismatched header is a HeaderMismatch.
    if header_str(headers, "mcp-protocol-version") != Some(body_pv) {
        return err(E_HEADER_MISMATCH, id, "MCP-Protocol-Version header missing or does not match body");
    }
    if header_str(headers, "mcp-method") != Some(method) {
        return err(E_HEADER_MISMATCH, id, "Mcp-Method header missing or does not match body");
    }

    // clientCapabilities is a required per-request field.
    let meta = msg.get("params").and_then(|p| p.get("_meta"));
    if meta.and_then(|m| m.get(META_CAPS)).is_none() {
        return err(E_INVALID_PARAMS, id, "missing io.modelcontextprotocol/clientCapabilities");
    }

    if !SUPPORTED_VERSIONS.contains(&body_pv) {
        return unsupported_version(id, body_pv);
    }

    match method {
        "tools/list" => ok_modern(id, json!({ "tools": listed_tools(ctx) })),
        "tools/call" => {
            let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if !name_header_ok(headers, name) {
                return err(E_HEADER_MISMATCH, id, "Mcp-Name header missing or does not match tool name");
            }
            if name.is_empty() {
                return err(E_INVALID_PARAMS, id, "missing tool name");
            }
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            ok_modern(id, tools::call(ctx, name, &args).await)
        }
        "ping" => ok_modern(id, json!({})),
        other => (
            StatusCode::NOT_FOUND,
            rpc_error(id, E_METHOD_NOT_FOUND, &format!("method not found: {other}")),
        ),
    }
}

/// Legacy (initialize-handshake) request: responses stay HTTP 200 with a
/// JSON-RPC body, as those clients expect.
async fn legacy(
    ctx: &Arc<McpCtx>,
    msg: &Value,
    headers: &HeaderMap,
    id: Value,
    method: &str,
) -> (StatusCode, Value) {
    // Explicit unsupported version header = 400 (2025-06-18+); absent is fine,
    // negotiated at initialize.
    if let Some(v) = header_str(headers, "mcp-protocol-version") {
        if !SUPPORTED_VERSIONS.contains(&v) {
            return (
                StatusCode::BAD_REQUEST,
                rpc_error(id, E_UNSUPPORTED_VERSION, &format!("unsupported protocol version: {v}")),
            );
        }
    }

    let result: Result<Value, (i64, String)> = match method {
        "initialize" => Ok(initialize_result(msg)),
        "tools/list" => Ok(json!({ "tools": listed_tools(ctx) })),
        "tools/call" => {
            let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if name.is_empty() {
                Err((E_INVALID_PARAMS, "missing tool name".into()))
            } else {
                let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                Ok(tools::call(ctx, name, &args).await)
            }
        }
        "ping" => Ok(json!({})),
        other => Err((E_METHOD_NOT_FOUND, format!("method not found: {other}"))),
    };

    match result {
        Ok(r) => (StatusCode::OK, json!({ "jsonrpc": "2.0", "id": id, "result": r })),
        Err((code, message)) => (StatusCode::OK, rpc_error(id, code, &message)),
    }
}

fn listed_tools(ctx: &McpCtx) -> Vec<Value> {
    tools::list(ctx.allow_write, ctx.allow_delete)
        .into_iter()
        .filter(|t| {
            t.get("name")
                .and_then(|n| n.as_str())
                .map(|n| !ctx.disabled_tools.iter().any(|d| d == n))
                .unwrap_or(true)
        })
        .collect()
}

fn initialize_result(msg: &Value) -> Value {
    let requested = msg
        .get("params")
        .and_then(|p| p.get("protocolVersion"))
        .and_then(|v| v.as_str());
    // Negotiates within the legacy set only: echo the client's version if
    // supported, else offer the newest legacy version.
    let pv = match requested {
        Some(v) if LEGACY_VERSIONS.contains(&v) => v,
        _ => LATEST_LEGACY,
    };
    json!({
        "protocolVersion": pv,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": server_info()
    })
}

fn discover_result(id: Value) -> (StatusCode, Value) {
    (
        StatusCode::OK,
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "resultType": "complete",
                "supportedVersions": SUPPORTED_VERSIONS,
                "capabilities": { "tools": { "listChanged": false } },
                "instructions": "Local S3 operations for the Cosmog desktop app.",
                "_meta": { META_SERVER_INFO: server_info() }
            }
        }),
    )
}

fn ok_modern(id: Value, mut result: Value) -> (StatusCode, Value) {
    if let Some(obj) = result.as_object_mut() {
        obj.entry("resultType").or_insert_with(|| json!("complete"));
        let meta = obj.entry("_meta").or_insert_with(|| json!({}));
        if let Some(m) = meta.as_object_mut() {
            m.insert(META_SERVER_INFO.into(), server_info());
        }
    }
    (StatusCode::OK, json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn server_info() -> Value {
    json!({ "name": "cosmog", "version": env!("CARGO_PKG_VERSION") })
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// Mcp-Name mirrors the tool name; it may be Base64-sentinel encoded.
fn name_header_ok(headers: &HeaderMap, expected: &str) -> bool {
    match header_str(headers, "mcp-name") {
        Some(v) => decode_header_value(v).as_deref() == Some(expected),
        None => false,
    }
}

/// Decode the `=?base64?...?=` sentinel form; pass plain values through.
fn decode_header_value(v: &str) -> Option<String> {
    match v.strip_prefix("=?base64?").and_then(|s| s.strip_suffix("?=")) {
        Some(inner) => {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(inner)
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
        }
        None => Some(v.to_string()),
    }
}

fn err(code: i64, id: Value, message: &str) -> (StatusCode, Value) {
    (StatusCode::BAD_REQUEST, rpc_error(id, code, message))
}

fn unsupported_version(id: Value, requested: &str) -> (StatusCode, Value) {
    (
        StatusCode::BAD_REQUEST,
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": E_UNSUPPORTED_VERSION,
                "message": "unsupported protocol version",
                "data": { "supported": SUPPORTED_VERSIONS, "requested": requested }
            }
        }),
    )
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn json_response(body: &Value) -> Response {
    json_status(StatusCode::OK, body)
}

fn json_status(status: StatusCode, body: &Value) -> Response {
    let text = serde_json::to_string(body).unwrap_or_else(|_| "{}".into());
    Response::builder()
        .status(status)
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

/// Loads the bearer token (minting one on first use); keyring access runs on
/// a blocking thread because it is synchronous.
pub async fn ensure_token() -> AppResult<String> {
    tokio::task::spawn_blocking(load_or_create_token)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
}

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

/// One advertised tool, shown in the settings UI.
#[derive(serde::Serialize)]
pub struct AdvertisedTool {
    pub name: String,
    pub description: String,
    pub enabled: bool,
}

/// Candidate tools for the given gates, flagged per the disable set; the UI
/// lists all of them so a disabled tool can be turned back on.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_negotiates_within_legacy_only() {
        // A known legacy version is echoed back.
        let req = json!({ "params": { "protocolVersion": "2025-06-18" } });
        assert_eq!(initialize_result(&req)["protocolVersion"], "2025-06-18");

        // A modern version over the handshake is not legacy: fall to newest legacy.
        let modern = json!({ "params": { "protocolVersion": "2026-07-28" } });
        assert_eq!(initialize_result(&modern)["protocolVersion"], LATEST_LEGACY);

        // No version requested also falls to newest legacy.
        let empty = json!({ "params": {} });
        assert_eq!(initialize_result(&empty)["protocolVersion"], LATEST_LEGACY);
    }

    #[test]
    fn discover_reports_all_versions_and_identity() {
        let (status, body) = discover_result(json!(1));
        assert_eq!(status, StatusCode::OK);
        let result = &body["result"];
        assert_eq!(result["resultType"], "complete");
        assert_eq!(result["supportedVersions"][0], "2026-07-28");
        assert!(result["supportedVersions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "2025-11-25"));
        assert_eq!(result["_meta"][META_SERVER_INFO]["name"], "cosmog");
    }

    #[test]
    fn unsupported_version_lists_supported_and_requested() {
        let (status, body) = unsupported_version(json!(7), "1900-01-01");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let error = &body["error"];
        assert_eq!(error["code"], E_UNSUPPORTED_VERSION);
        assert_eq!(error["data"]["requested"], "1900-01-01");
        assert_eq!(error["data"]["supported"][0], "2026-07-28");
    }

    #[test]
    fn ok_modern_stamps_result_type_and_server_info() {
        let (status, body) = ok_modern(json!(3), json!({ "content": [] }));
        assert_eq!(status, StatusCode::OK);
        let result = &body["result"];
        assert_eq!(result["resultType"], "complete");
        assert_eq!(result["_meta"][META_SERVER_INFO]["name"], "cosmog");
        // Existing fields survive.
        assert!(result["content"].is_array());
    }

    #[test]
    fn ok_modern_preserves_an_explicit_result_type() {
        let (_, body) = ok_modern(json!(4), json!({ "resultType": "input_required" }));
        assert_eq!(body["result"]["resultType"], "input_required");
    }

    #[test]
    fn decode_header_value_handles_plain_and_base64_sentinel() {
        assert_eq!(decode_header_value("s3_object_upload").as_deref(), Some("s3_object_upload"));
        assert_eq!(
            decode_header_value("=?base64?SGVsbG8sIOS4lueVjA==?=").as_deref(),
            Some("Hello, 世界")
        );
        assert!(decode_header_value("=?base64?not valid!?=").is_none());
    }

    #[test]
    fn name_header_matches_only_the_mirrored_tool_name() {
        let mut headers = HeaderMap::new();
        headers.insert("mcp-name", "s3_objects_list".parse().unwrap());
        assert!(name_header_ok(&headers, "s3_objects_list"));
        assert!(!name_header_ok(&headers, "s3_object_delete"));
        assert!(!name_header_ok(&HeaderMap::new(), "s3_objects_list"));
    }
}
