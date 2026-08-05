//! Access guards for the local MCP endpoint.
//!
//! Localhost binding is not isolation on its own: any local process can reach
//! the port, and a browser page can try to drive it via DNS rebinding. So we
//! validate the Origin/Host headers and require a bearer token on every call.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;

use super::McpCtx;

/// Reject anything without the bearer token or with a non-local Origin.
pub async fn guard(State(ctx): State<Arc<McpCtx>>, req: Request, next: Next) -> Response {
    let headers = req.headers();

    // Origin, when present, must be local. Non-browser clients send none, which
    // is fine. A cross-site browser page sends its own origin and is rejected:
    // the DNS-rebinding defense.
    if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) {
        if !origin_is_local(origin) {
            return deny(StatusCode::FORBIDDEN, "origin not allowed");
        }
    }

    // Host must resolve to loopback so a rebinding host cannot slip through.
    if let Some(host) = headers.get("host").and_then(|v| v.to_str().ok()) {
        if !host_is_local(host) {
            return deny(StatusCode::FORBIDDEN, "host not allowed");
        }
    }

    let ok = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| token_eq(t, &ctx.token))
        .unwrap_or(false);
    if !ok {
        return deny(StatusCode::UNAUTHORIZED, "missing or invalid bearer token");
    }

    next.run(req).await
}

fn origin_is_local(origin: &str) -> bool {
    // "null" (sandboxed iframe, file://, opaque origin) is not local, reject it.
    let rest = match origin.split_once("://") {
        Some((scheme, rest)) if scheme == "http" || scheme == "https" => rest,
        _ => return false,
    };
    host_is_local(rest)
}

fn host_is_local(host: &str) -> bool {
    // Strip a trailing port.
    let hostname = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    let hostname = hostname.trim_start_matches('[').trim_end_matches(']');
    matches!(hostname, "localhost" | "127.0.0.1" | "::1")
}

/// Length-checked equality. Avoids leaking length via early return only.
fn token_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

fn deny(code: StatusCode, msg: &str) -> Response {
    tracing::warn!(status = code.as_u16(), "mcp request rejected: {msg}");
    Response::builder()
        .status(code)
        .body(msg.to_string().into())
        .unwrap_or_else(|_| Response::new(String::new().into()))
}
