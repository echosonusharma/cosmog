//! MCP tools calling the same `AppState` methods as the Tauri commands. Reads
//! are always advertised; writes/deletes gate on settings. Encrypted buckets and presigned URLs are out of scope for v1.

use serde_json::{json, Value};

use crate::db::cache::{SearchQuery, SearchScope};
use crate::db::transfers::TransferOrigin;
use crate::error::AppError;
use crate::store::{GetOptions, ListOptions, PutOptions};
use crate::transfer::ProgressSink;
use crate::validate;

use super::format;
use super::McpCtx;

pub fn list(allow_write: bool, allow_delete: bool) -> Vec<Value> {
    let mut tools = vec![
        def(
            "s3_accounts_list",
            "List configured storage accounts (id, name, protocol, endpoint, region). Credentials are never returned.",
            schema(json!({}), &[]),
            true,
            false,
        ),
        def(
            "s3_buckets_list",
            "List buckets for an account.",
            schema(
                json!({ "account_id": pstr("Account id from s3_accounts_list") }),
                &["account_id"],
            ),
            true,
            false,
        ),
        def(
            "s3_objects_list",
            "List objects and folders under a prefix. Returns compact CSV, paginated. Pass next_cursor to continue.",
            schema(
                json!({
                    "account_id": pstr("Account id"),
                    "bucket_name": pstr("Bucket name"),
                    "prefix": pstr("Key prefix to list under. Empty lists the root."),
                    "delimiter": pstr("Folder delimiter. Defaults to '/'. Pass empty string for a flat recursive listing."),
                    "limit": pint("Max rows per page (1..1000, default 200)"),
                    "cursor": pstr("Continuation token from a prior next_cursor")
                }),
                &["account_id", "bucket_name"],
            ),
            true,
            false,
        ),
        def(
            "s3_objects_search",
            "Full-text search the local object index for a bucket. Requires the bucket to be indexed in the app.",
            schema(
                json!({
                    "account_id": pstr("Account id"),
                    "bucket_name": pstr("Bucket name"),
                    "query": pstr("Search text. Omit to browse with filters only."),
                    "prefix": pstr("Restrict to this prefix. Omit to search the whole bucket."),
                    "recursive": pbool("When prefix is set, match everything under it (default true)"),
                    "limit": pint("Max rows per page (default 200)"),
                    "cursor": pint("Numeric cursor from a prior next_cursor")
                }),
                &["account_id", "bucket_name"],
            ),
            true,
            false,
        ),
        def(
            "s3_object_head",
            "Fetch metadata for a single object.",
            schema(
                json!({
                    "account_id": pstr("Account id"),
                    "bucket_name": pstr("Bucket name"),
                    "object_key": pstr("Full object key")
                }),
                &["account_id", "bucket_name", "object_key"],
            ),
            true,
            false,
        ),
        def(
            "s3_bucket_stats",
            "Aggregate stats for a bucket from the local index: object count, total size, top extensions and storage classes.",
            schema(
                json!({
                    "account_id": pstr("Account id"),
                    "bucket_name": pstr("Bucket name")
                }),
                &["account_id", "bucket_name"],
            ),
            true,
            false,
        ),
        def(
            "s3_transfer_status",
            "Check the status of an upload or download by its transfer_id.",
            schema(
                json!({ "transfer_id": pstr("Transfer id returned by upload or download") }),
                &["transfer_id"],
            ),
            true,
            false,
        ),
    ];

    if allow_write {
        tools.push(def(
            "s3_object_upload",
            "Queue an upload of a local file to an object key. Returns a transfer_id to poll with s3_transfer_status.",
            schema(
                json!({
                    "account_id": pstr("Account id"),
                    "bucket_name": pstr("Bucket name"),
                    "object_key": pstr("Destination object key"),
                    "local_path": pstr("Absolute path of the local file to upload")
                }),
                &["account_id", "bucket_name", "object_key", "local_path"],
            ),
            false,
            false,
        ));
        tools.push(def(
            "s3_object_download",
            "Queue a download of an object to a local path. Returns a transfer_id to poll with s3_transfer_status.",
            schema(
                json!({
                    "account_id": pstr("Account id"),
                    "bucket_name": pstr("Bucket name"),
                    "object_key": pstr("Object key to download"),
                    "local_path": pstr("Absolute destination path on disk")
                }),
                &["account_id", "bucket_name", "object_key", "local_path"],
            ),
            false,
            false,
        ));
    }

    if allow_delete {
        tools.push(def(
            "s3_object_delete",
            "Permanently delete a single object. Destructive and not reversible.",
            schema(
                json!({
                    "account_id": pstr("Account id"),
                    "bucket_name": pstr("Bucket name"),
                    "object_key": pstr("Object key to delete")
                }),
                &["account_id", "bucket_name", "object_key"],
            ),
            false,
            true,
        ));
    }

    tools
}

/// Tool-call dispatch: always CallToolResult-shaped; failures come back as
/// isError results, never protocol errors.
pub async fn call(ctx: &McpCtx, name: &str, args: &Value) -> Value {
    let account = args.get("account_id").and_then(|a| a.as_str()).unwrap_or("");
    tracing::info!(tool = name, account, "mcp tool call");
    let out = dispatch(ctx, name, args).await;
    if out.get("isError").and_then(|v| v.as_bool()).unwrap_or(false) {
        tracing::warn!(tool = name, "mcp tool call returned error");
    } else {
        tracing::debug!(tool = name, "mcp tool call ok");
    }
    out
}

async fn dispatch(ctx: &McpCtx, name: &str, args: &Value) -> Value {
    if ctx.disabled_tools.iter().any(|d| d == name) {
        return format::error(format!("tool {name} is disabled in the MCP settings."));
    }
    if let Some(acct) = args.get("account_id").and_then(|a| a.as_str()) {
        if ctx.disabled_accounts.iter().any(|d| d == acct) {
            return format::error(format!(
                "account {acct} is disabled for MCP access. Enable it in the MCP settings."
            ));
        }
    }
    match name {
        "s3_accounts_list" => run(accounts_list(ctx)).await,
        "s3_buckets_list" => run(buckets_list(ctx, args)).await,
        "s3_objects_list" => run(objects_list(ctx, args)).await,
        "s3_objects_search" => run(objects_search(ctx, args)).await,
        "s3_object_head" => run(object_head(ctx, args)).await,
        "s3_bucket_stats" => run(bucket_stats(ctx, args)).await,
        "s3_transfer_status" => run(transfer_status(ctx, args)).await,
        "s3_object_upload" => {
            if !ctx.allow_write {
                return format::error("uploads are disabled. Enable write access in the MCP settings.");
            }
            run(object_upload(ctx, args)).await
        }
        "s3_object_download" => {
            if !ctx.allow_write {
                return format::error("downloads are disabled. Enable write access in the MCP settings.");
            }
            run(object_download(ctx, args)).await
        }
        "s3_object_delete" => {
            if !ctx.allow_delete {
                return format::error("deletes are disabled. Enable delete access in the MCP settings.");
            }
            run(object_delete(ctx, args)).await
        }
        other => format::error(format!("unknown tool: {other}")),
    }
}

// Handler Err values already hold isError results; `run` unwraps to the wire value.
async fn run(fut: impl std::future::Future<Output = Result<Value, Value>>) -> Value {
    fut.await.unwrap_or_else(|e| e)
}

async fn accounts_list(ctx: &McpCtx) -> Result<Value, Value> {
    let accounts = ctx.state.db.list_accounts().await.map_err(map_err)?;
    let rows: Vec<Vec<String>> = accounts
        .iter()
        .filter(|a| !ctx.disabled_accounts.iter().any(|d| d == &a.id))
        .map(|a| {
            vec![
                a.id.clone(),
                a.name.clone(),
                a.protocol.clone(),
                a.endpoint.clone().unwrap_or_default(),
                a.region.clone(),
            ]
        })
        .collect();
    let (csv, rendered, budget) = format::csv_table(&["id", "name", "protocol", "endpoint", "region"], &rows);
    Ok(format::listing(csv, rendered, false, None, budget))
}

async fn buckets_list(ctx: &McpCtx, args: &Value) -> Result<Value, Value> {
    let account = sreq(args, "account_id")?;
    let store = ctx.state.store_for(&account).await.map_err(map_err)?;
    let buckets = store.list_buckets().await.map_err(map_err)?;
    let rows: Vec<Vec<String>> = buckets
        .iter()
        .map(|b| vec![b.name.clone(), opt_i64(b.created_at)])
        .collect();
    let (csv, rendered, budget) = format::csv_table(&["name", "created_at"], &rows);
    Ok(format::listing(csv, rendered, false, None, budget))
}

async fn objects_list(ctx: &McpCtx, args: &Value) -> Result<Value, Value> {
    let account = sreq(args, "account_id")?;
    let bucket = sreq(args, "bucket_name")?;
    let prefix = sopt(args, "prefix");
    // Absent delimiter defaults to "/"; explicit empty string = flat recursive.
    let delimiter = match args.get("delimiter") {
        Some(v) => v.as_str().filter(|s| !s.is_empty()).map(|s| s.to_string()),
        None => Some("/".to_string()),
    };
    let limit = iopt(args, "limit").unwrap_or(200).clamp(1, 1000) as i32;
    let cursor = sopt(args, "cursor");

    let store = ctx.state.store_for(&account).await.map_err(map_err)?;
    let page = store
        .list_objects(
            &bucket,
            ListOptions {
                prefix,
                delimiter,
                continuation: cursor,
                max_keys: Some(limit),
            },
        )
        .await
        .map_err(map_err)?;

    let mut rows: Vec<Vec<String>> = Vec::new();
    for p in &page.prefixes {
        rows.push(vec![p.clone(), String::new(), String::new(), "DIR".into()]);
    }
    for o in &page.objects {
        rows.push(vec![
            o.key.clone(),
            o.size.to_string(),
            opt_i64(o.last_modified),
            o.storage_class.clone().unwrap_or_default(),
        ]);
    }
    let (csv, rendered, budget) =
        format::csv_table(&["key", "size", "last_modified", "storage_class"], &rows);
    Ok(format::listing(csv, rendered, page.is_truncated, page.continuation, budget))
}

async fn objects_search(ctx: &McpCtx, args: &Value) -> Result<Value, Value> {
    let account = sreq(args, "account_id")?;
    let bucket = sreq(args, "bucket_name")?;
    let query = sopt(args, "query");
    let prefix = sopt(args, "prefix");
    let recursive = bopt(args, "recursive").unwrap_or(true);
    let limit = iopt(args, "limit").unwrap_or(200).clamp(1, 1000) as u32;
    let cursor = iopt(args, "cursor");

    let scope = match prefix {
        Some(p) => SearchScope::Prefix { prefix: p, recursive },
        None => SearchScope::Bucket,
    };
    let q = SearchQuery {
        account_id: account,
        bucket,
        scope,
        query,
        filters: Default::default(),
        sort: Default::default(),
        sort_dir: Default::default(),
        page_size: Some(limit),
        cursor,
    };
    let res = ctx.state.db.search_objects(q).await.map_err(map_err)?;
    let rows: Vec<Vec<String>> = res
        .objects
        .iter()
        .map(|o| {
            vec![
                o.key.clone(),
                o.size.to_string(),
                opt_i64(o.last_modified),
                o.storage_class.clone().unwrap_or_default(),
            ]
        })
        .collect();
    let (csv, rendered, budget) =
        format::csv_table(&["key", "size", "last_modified", "storage_class"], &rows);
    let next = res.next_cursor.map(|c| c.to_string());
    Ok(format::listing(csv, rendered, next.is_some(), next, budget))
}

async fn object_head(ctx: &McpCtx, args: &Value) -> Result<Value, Value> {
    let account = sreq(args, "account_id")?;
    let bucket = sreq(args, "bucket_name")?;
    let key = sreq(args, "object_key")?;
    let store = ctx.state.store_for(&account).await.map_err(map_err)?;
    let m = store.head_object(&bucket, &key).await.map_err(map_err)?;
    let mut out = String::new();
    out.push_str(&format!("key: {}\n", m.key));
    out.push_str(&format!("size: {}\n", m.size));
    out.push_str(&format!("last_modified: {}\n", opt_i64(m.last_modified)));
    out.push_str(&format!("etag: {}\n", m.etag.unwrap_or_default()));
    out.push_str(&format!("content_type: {}\n", m.content_type.unwrap_or_default()));
    out.push_str(&format!("storage_class: {}\n", m.storage_class.unwrap_or_default()));
    out.push_str(&format!("version_id: {}\n", m.version_id.unwrap_or_default()));
    for (k, v) in &m.user_metadata {
        out.push_str(&format!("meta.{k}: {v}\n"));
    }
    Ok(format::text(out))
}

async fn bucket_stats(ctx: &McpCtx, args: &Value) -> Result<Value, Value> {
    let account = sreq(args, "account_id")?;
    let bucket = sreq(args, "bucket_name")?;
    let s = ctx.state.db.bucket_stats(&account, &bucket).await.map_err(map_err)?;
    let mut out = String::new();
    out.push_str(&format!("object_count: {}\n", s.object_count));
    out.push_str(&format!("total_bytes: {}\n", s.total_bytes));
    out.push_str(&format!("distinct_extensions: {}\n", s.extension_count));
    out.push_str("top_storage_classes:\n");
    for sc in s.by_storage_class.iter().take(10) {
        out.push_str(&format!("  {} count={} bytes={}\n", sc.storage_class, sc.object_count, sc.total_bytes));
    }
    out.push_str("top_extensions:\n");
    for ext in s.by_extension.iter().take(10) {
        out.push_str(&format!("  {} count={} bytes={}\n", ext.extension, ext.object_count, ext.total_bytes));
    }
    out.push_str("largest_objects:\n");
    for lo in s.largest.iter().take(10) {
        out.push_str(&format!("  {} bytes={}\n", lo.key, lo.size));
    }
    Ok(format::text(out))
}

async fn transfer_status(ctx: &McpCtx, args: &Value) -> Result<Value, Value> {
    let id = sreq(args, "transfer_id")?;
    let t = ctx.state.transfers.get(&id).await.map_err(map_err)?;
    let mut out = String::new();
    out.push_str(&format!("transfer_id: {}\n", t.id));
    out.push_str(&format!("direction: {}\n", t.direction.as_str()));
    out.push_str(&format!("status: {}\n", t.status.as_str()));
    out.push_str(&format!("bucket: {}\n", t.bucket));
    out.push_str(&format!("key: {}\n", t.key));
    out.push_str(&format!("bytes_done: {}\n", t.bytes_done));
    out.push_str(&format!("bytes_total: {}\n", opt_i64(t.bytes_total)));
    if let Some(e) = t.error {
        out.push_str(&format!("error: {e}\n"));
    }
    Ok(format::text(out))
}

async fn object_upload(ctx: &McpCtx, args: &Value) -> Result<Value, Value> {
    let account = sreq(args, "account_id")?;
    let bucket = sreq(args, "bucket_name")?;
    let key = sreq(args, "object_key")?;
    let local_path = sreq(args, "local_path")?;
    refuse_if_encrypted(ctx, &account, &bucket).await?;
    let path = validate::validate_upload_source(&local_path).await.map_err(map_err)?;
    // Confine the source to the configured MCP folder: object keys are
    // untrusted, so an injected key could exfiltrate any file.
    contained_existing(ctx, &path).await?;
    let store = ctx.state.store_for(&account).await.map_err(map_err)?;
    let id = ctx
        .state
        .transfers
        .enqueue_upload(
            store,
            account,
            bucket,
            key,
            path,
            PutOptions::default(),
            ProgressSink::noop(),
            TransferOrigin::User,
        )
        .await
        .map_err(map_err)?;
    Ok(format::text(format!(
        "transfer_id: {id}\nstatus: queued\nPoll s3_transfer_status with this id."
    )))
}

async fn object_download(ctx: &McpCtx, args: &Value) -> Result<Value, Value> {
    let account = sreq(args, "account_id")?;
    let bucket = sreq(args, "bucket_name")?;
    let key = sreq(args, "object_key")?;
    let local_path = sreq(args, "local_path")?;
    refuse_if_encrypted(ctx, &account, &bucket).await?;
    let dest = validate::validate_download_dest(&local_path).await.map_err(map_err)?;
    // Confine the destination to the MCP folder so downloads can't overwrite
    // arbitrary files on disk.
    contained_dest(ctx, &dest).await?;
    let store = ctx.state.store_for(&account).await.map_err(map_err)?;
    let id = ctx
        .state
        .transfers
        .enqueue_download(store, account, bucket, key, dest, GetOptions::default(), ProgressSink::noop())
        .await
        .map_err(map_err)?;
    Ok(format::text(format!(
        "transfer_id: {id}\nstatus: queued\nPoll s3_transfer_status with this id."
    )))
}

async fn object_delete(ctx: &McpCtx, args: &Value) -> Result<Value, Value> {
    let account = sreq(args, "account_id")?;
    let bucket = sreq(args, "bucket_name")?;
    let key = sreq(args, "object_key")?;
    // Encrypted buckets NOT refused here: delete touches opaque bytes only, no
    // keychain identity; only upload/download (cleartext) are gated.
    let store = ctx.state.store_for(&account).await.map_err(map_err)?;
    // S3 DELETE "succeeds" even for missing keys, lying to the model; head
    // first so a missing key surfaces as explicit not-found.
    store.head_object(&bucket, &key).await.map_err(map_err)?;
    store.delete_object(&bucket, &key).await.map_err(map_err)?;
    let _ = ctx.state.db.cache_remove_object(&account, &bucket, &key).await;
    Ok(format::text(format!("deleted: {key}")))
}

/// Confines an existing upload source to the sandbox root, canonicalizing to
/// defeat symlink/`..` escapes.
async fn contained_existing(ctx: &McpCtx, path: &std::path::Path) -> Result<(), Value> {
    let root = require_root(ctx)?;
    let canon = tokio::fs::canonicalize(path)
        .await
        .map_err(|e| format::error(format!("local_path: {e}")))?;
    within(root, &canon)
}

/// Confines a download destination: existing paths (planted symlinks included)
/// resolve fully; new files confine their existing parent instead.
async fn contained_dest(ctx: &McpCtx, dest: &std::path::Path) -> Result<(), Value> {
    let root = require_root(ctx)?;
    if tokio::fs::symlink_metadata(dest).await.is_ok() {
        let canon = tokio::fs::canonicalize(dest)
            .await
            .map_err(|e| format::error(format!("local_path: {e}")))?;
        return within(root, &canon);
    }
    let parent = dest
        .parent()
        .ok_or_else(|| format::error("local_path has no parent directory"))?;
    let canon = tokio::fs::canonicalize(parent)
        .await
        .map_err(|e| format::error(format!("local_path parent: {e}")))?;
    within(root, &canon)
}

fn require_root(ctx: &McpCtx) -> Result<&std::path::Path, Value> {
    ctx.fs_root.as_deref().ok_or_else(|| {
        format::error(
            "file transfers are disabled: set an allowed folder (MCP file root) in the MCP settings first.",
        )
    })
}

fn within(root: &std::path::Path, path: &std::path::Path) -> Result<(), Value> {
    if path.starts_with(root) {
        Ok(())
    } else {
        Err(format::error(format!(
            "local_path is outside the allowed MCP folder ({}). Choose a path inside it.",
            root.display()
        )))
    }
}

/// Refuses data ops on encrypted buckets: v1 has no keychain identity in the MCP path.
async fn refuse_if_encrypted(ctx: &McpCtx, account: &str, bucket: &str) -> Result<(), Value> {
    match ctx.state.db.get_encryption_config(account, bucket).await {
        Ok(Some(_)) => Err(format::error(
            "unsupported in MCP v1: this bucket has client-side encryption enabled.",
        )),
        Ok(None) => Ok(()),
        Err(e) => Err(map_err(e)),
    }
}

fn sreq(args: &Value, key: &str) -> Result<String, Value> {
    match args.get(key).and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => Ok(s.to_string()),
        _ => Err(format::error(format!("missing required argument: {key}"))),
    }
}

fn sopt(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn iopt(args: &Value, key: &str) -> Option<i64> {
    args.get(key).and_then(|v| v.as_i64())
}

fn bopt(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}

fn opt_i64(v: Option<i64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_default()
}

fn map_err(e: AppError) -> Value {
    let msg = match &e {
        AppError::Archived(m) => format!(
            "object is in an archived storage class and cannot be read directly. Restore it to a standard tier first. ({m})"
        ),
        AppError::NotFound(m) => format!("not found: {m}"),
        AppError::AccessDenied(m) => format!("access denied: {m}"),
        _ => e.to_string(),
    };
    format::error(msg)
}

fn def(name: &str, description: &str, input_schema: Value, read_only: bool, destructive: bool) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
        "annotations": {
            "title": name,
            "readOnlyHint": read_only,
            "destructiveHint": destructive
        }
    })
}

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required
    })
}

fn pstr(desc: &str) -> Value {
    json!({ "type": "string", "description": desc })
}

fn pint(desc: &str) -> Value {
    json!({ "type": "integer", "description": desc })
}

fn pbool(desc: &str) -> Value {
    json!({ "type": "boolean", "description": desc })
}
