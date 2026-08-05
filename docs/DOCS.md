# Cosmog - Developer Docs

Desktop and Android app for managing S3-compatible object storage. v0.1.25.

---

## Tech Stack

### Frontend

| Tech | Purpose |
|------|---------|
| Solid.js 1.9 | Reactive UI |
| TypeScript 5.6 | Type safety |
| Vite 6 | Build tool / dev server |
| Tauri 2 | Native bridge (IPC, commands) |
| CodeMirror 6 | Text editor with syntax highlighting |
| ExcelJS | Spreadsheet parse / edit |
| pdfjs-dist 6 | PDF rendering (legacy build for WebKit compat) |
| TanStack Solid Virtual | Virtualized list rendering |
| uPlot | Storage-analytics time-series charts |

### Backend (Rust)

| Crate | Purpose |
|-------|---------|
| tauri 2 | App runtime and command bridge |
| tokio | Async runtime |
| axum 0.8 | Local MCP server HTTP endpoint (desktop only) |
| aws-sdk-s3 / aws-config | S3 API |
| tokio-rusqlite / rusqlite | SQLite (WAL mode, FTS5) |
| keyring 3 | OS keychain (Apple / Windows / Linux Secret Service) |
| age 0.11 | Client-side encryption (X25519 + ChaCha20-Poly1305 streaming) |
| zeroize | Best-effort scrub of key material |
| serde / serde_json | Serialization |
| tracing + tracing-appender | Structured logging, rolling files |
| thiserror / anyhow | Error handling |

**Tauri plugins:** `dialog`, `fs`, `notification`, `opener`, `single-instance`, `autostart` (desktop background-run)

### Android (Kotlin / JNI)

| Component | Purpose |
|-----------|---------|
| `MainActivity.kt` | Tauri entry point |
| `CosmogApp.kt` | Application subclass; per-process native init (ndk_context + JNI class cache). Runs in every process. |
| `TransferService.kt` | Foreground service (dataSync); keeps transfers alive when backgrounded |
| `NightWatchService.kt` | Foreground service in its own `:nightwatch` process; hosts the headless Night Watcher sync loop (see below) |
| `BootReceiver.kt` | Re-arms Night Watcher after reboot (defers the dataSync FGS start to first foreground on Android 12+) |
| `NwTreePicker.kt` | SAF tree picker (`ACTION_OPEN_DOCUMENT_TREE`) bridge, polled from Rust |
| `SecretStore.kt` | EncryptedSharedPreferences backed by Android Keystore |
| `saf.rs` | JNI bridge for Storage Access Framework (upload staging, download finalize, delete placeholder, SAF tree walk) |
| `night_watcher_headless.rs` | Headless Rust sync host for the `:nightwatch` process (no Tauri/webview) |

---

## Architecture

```
Frontend (Solid.js)
    | invoke()
    v
Tauri Commands (src-tauri/src/commands/)
    |
    v
AppState (Arc, shared across all commands)
  +-- TransferManager  →  ObjectStore trait  →  S3Store  →  S3 API
  +-- Db (SQLite)      →  accounts, transfers, cache, settings, capabilities, encryption
  +-- Secrets          →  OS Keyring / Android Keystore (never in SQLite)
  +-- crypto           →  age v1 streaming encrypt/decrypt (upload/download/preview)
```

**Rules:**
- `AppState` is `Arc`-cloned into every command - cheap
- Secrets never touch SQLite
- Schema: append-only migrations array, never reorder
- `ObjectStore` trait = only provider abstraction; commands are protocol-agnostic

---

## Directory Structure

```
cosmog/
+-- src/                        # Frontend (TypeScript / Solid.js)
|   +-- api/                    # Tauri command wrappers
|   +-- routes/
|   |   +-- browse/
|   |   |   +-- preview/        # SheetModal, PdfModal, Lightbox, MetaList
|   |   |   +-- charts/          # Donut (SVG), TimeSeriesChart (uPlot)
|   |   |   +-- StatsModal, ObjectBrowser, PreviewPane, ColumnPane, ListView, ...
|   |   +-- MainApp, Settings, Transfers, Logs, Onboarding, ...
|   +-- state/                  # Solid.js signal stores
|   +-- styles/                 # CSS files (no inline styles)
|
+-- src-tauri/
    +-- src/
    |   +-- commands/           # Tauri command handlers (one file per domain)
    |   +-- db/                 # SQLite schema + domain methods
    |   +-- store/              # ObjectStore trait + S3Store
    |   +-- transfer/           # TransferManager, worker pool
    |   +-- crypto.rs           # age streaming encrypt/decrypt + magic probe
    |   +-- saf.rs              # Android JNI: SAF upload/download/delete + tree walk
    |   +-- night_watcher.rs    # One-way folder->bucket sync core (reconcile loop, NwCtx trait)
    |   +-- night_watcher_headless.rs  # Android: headless sync host for the :nightwatch process
    |   +-- mcp/                # Local MCP server (desktop only): mod (JSON-RPC + token), auth, tools, format
    |   +-- app_lifecycle.rs    # Desktop background-run (tray, close-to-hide, autostart)
    |   +-- scheduler.rs        # Auto-reindex background loop
    |   +-- secrets.rs          # OS keyring read/write
    |   +-- state.rs            # AppState
    |   +-- device.rs           # Device info (Android platform detection)
    +-- gen/android/            # Generated Android project (committed to git)
        +-- app/src/main/
            +-- AndroidManifest.xml   # portrait lock, configChanges
            +-- java/com/sonus/cosmog/
```

---

## Tauri Command Surface

### Accounts
`add_account`, `list_accounts`, `get_account`, `update_account`, `delete_account`, `test_account`, `detect_account_region`

### Buckets
`list_buckets`, `create_bucket`, `delete_bucket`, `head_bucket`, `get_bucket_location`, `put_bucket_acl`, `get_bucket_versioning`, `put_bucket_versioning`, `list_multipart_uploads`, `cleanup_stale_multiparts`, `abort_multipart_upload`

### Objects
`list_objects`, `head_object`, `create_folder`, `delete_object`, `delete_objects`, `delete_object_version`, `list_object_versions`, `copy_object`, `move_object`, `put_object_acl`, `get_object_tagging`, `put_object_tagging`, `delete_object_tagging`, `presign_get`, `preview_object`, `put_object_text`, `put_object_bytes_cmd`, `list_keys_under_prefix`

### Transfers
`enqueue_upload`, `enqueue_download`, `list_transfers`, `get_transfer`, `cancel_transfer`, `retry_transfer`, `clear_completed_transfers`, `clear_transfer`

### Search / Index
`search_objects`, `sync_prefix`, `bucket_index_status`, `enable_bucket_index`, `cancel_bucket_scan`, `reindex_bucket`, `disable_bucket_index`, `bucket_stats`, `set_bucket_auto_reindex`

### Bulk Ops
`delete_folder_cmd`, `upload_directory_cmd`, `download_directory_cmd`, `cancel_bulk_op`

### Capabilities
`probe_account_capabilities`, `probe_bucket_capabilities`, `get_account_capabilities`, `get_bucket_capabilities`

### Portable (Backup / Restore)
`export_config`, `import_config`, `backup_database`, `stage_restore`, `clear_app_data`

### Encryption (per-bucket, client-side)
`enable_bucket_encryption`, `disable_bucket_encryption`, `get_bucket_encryption_status`, `export_encryption_key`, `save_encryption_key_export`, `import_encryption_identity`, `import_encryption_identity_from_file`, `has_encryption_identity`

### Night Watcher
`nw_list_watches`, `nw_add_watch`, `nw_update_watch`, `nw_delete_watch`, `nw_set_watch_enabled`, `nw_get_status`, `nw_pick_tree` (Android SAF tree picker), `nw_quit_background` (desktop)

### MCP (desktop only)
`mcp_get_config`, `mcp_set_config`, `mcp_regenerate_token`, `mcp_status`

### Android only
`notify_ex`, `set_transfer_service`, `stage_saf_upload`, `finalize_saf_download`, `delete_saf_document`, `query_display_name`

### Misc
`get_settings`, `update_settings`, `reset_settings`, `get_log_dir`, `get_log_tail`, `browse_prefix`

---

## Transfer Engine

Events: `Started`, `Progress`, `PartCompleted`, `Done`, `Failed`, `Canceled`

Key behaviors:
- Multipart upload with part-level resume on retry
- `CancellationToken` per transfer
- Orphan transfers (Active/Pending at crash) reaped at startup (desktop only; on Android the `:nightwatch` process may hold genuinely-live rows the blind reap would clobber)
- Encrypted buckets: uploads stream through age to `enc_tmp/<uuid>.age`, cleaned after worker settles; downloads probe age magic then stream-decrypt in place
- Retry on encrypted downloads always re-fetches the full range (age requires full stream to authenticate)

**Android:** `TransferService` starts when any transfer becomes active, stops when none remain. `START_NOT_STICKY`. Wakelock held for service lifetime.

**SAF download flow:** frontend registers `(transfer_id, SAF URI)` before enqueue. After `Done`, `finalize_saf_download` copies cache file to SAF URI via JNI, then deletes cache file. On cancel, 0-byte SAF placeholder deleted via `delete_saf_document`.

---

## Client-side Encryption

Per-bucket, transparent, uses [age file format](https://age-encryption.org) (X25519 + streaming ChaCha20-Poly1305, 64 KiB chunks).

- Secret key (`AGE-SECRET-KEY-…`) in OS keychain under `enc:<account_id>:<bucket>`
- Public recipient (`age1…`) in SQLite `bucket_encryption` table
- Rotate destroys previous key irreversibly - FE must walk user through export first
- `presign_get` refuses to generate URLs for encrypted buckets unless `allow_ciphertext=true`
- `enc_tmp/` is swept unconditionally at startup
- Exported key is compatible with `age -d -i keyfile.txt <ciphertext>`

**Limits:** in-memory helpers cap at 512 MiB; preview cap at 128 MiB; file streaming unbounded (disk-limited).

---

## Night Watcher

One-way background sync: mirror a local directory to an S3 prefix. A locally
deleted file only drops its state row (`delete_policy = "keep"`); the remote
object is never deleted.

**Core (`night_watcher.rs`, cross-platform):** a periodic full scan is the
source of truth. Per watch, when `last_scan_at + full_scan_secs` has elapsed, it
walks the tree and reconciles each file. Change detection is a cheap `mtime + size`
fast-path; only on a miss is the file hashed (blake3) to decide if content
actually changed. Changed files feed the existing encrypt/enqueue upload path;
`nw_file_state` records the synced fingerprint on upload completion. The
reconcile core is generic over an `NwCtx` trait (Db + TransferManager + store
cache + claim sets) so it runs without a `tauri::AppHandle`.

**Desktop:** the loop runs in-process (`night_watcher::spawn`), plus a `notify`
filesystem watcher as a near-instant accelerator. The process is kept alive
after the window closes via `app_lifecycle.rs` (tray + close-to-hide +
`autostart --hidden`), gated on there being at least one enabled watch. On a
host with no system tray, closing the window quits (background sync stops).

**Android:** arbitrary user dirs are SAF `content://` trees (no inotify, no
real path), so the loop relies on the periodic scan. It runs in a dedicated
`NightWatchService` **process** (`android:process=":nightwatch"`), NOT the main
Tauri process: wry calls `std::process::exit(0)` when the Activity is destroyed
(swipe / reclaim), which would kill an in-process loop and its foreground
service. `night_watcher_headless.rs` (JNI `startNwSync` / `stopNwSync`) builds a
headless `NwCtx` on its own tokio runtime and drives the same reconcile core.

- DB path resolved via JNI `context.getDataDir()` (the same call Tauri's
  `app_data_dir()` makes), so both processes open the same sqlite file. WAL +
  `busy_timeout` cover the two-process access. The main process does NOT run the
  loop on Android, so `nw_file_state` has a single writer.
- SAF scan enumerates the tree via `DocumentsContract` (JNI in `saf.rs`);
  changed documents are staged to a real fs path under `<data_dir>/nw_stage/`
  before upload. Persisted tree URI permission is package-scoped, so the service
  process can read it.
- `NwTreePicker` fires `ACTION_OPEN_DOCUMENT_TREE` from `MainActivity`, polled by
  `nw_pick_tree`. `START_STICKY` so an LMK-killed service process is recreated;
  `BootReceiver` re-arms after reboot.
- Any Kotlin class reached from Rust via JNI needs a proguard `-keep` rule (R8
  cannot see JNI call sites).

---

## MCP Server

Desktop only (`#[cfg(not(target_os = "android"))]` on the module, commands, and
the sidebar nav). Lets a local AI client (Claude Code/Desktop, Cursor, Codex,
VS Code, Windsurf) drive S3 ops. Off by default.

**One process, one AppState.** MCP tools call the same `AppState` methods the
Tauri commands do, so there is one `TransferManager` and one SQLite writer, no
second daemon. The listener is a tokio task inside the app process.

**Transport (`mcp/mod.rs`).** Hand-rolled JSON-RPC 2.0 over an axum 0.8 POST
handler at `127.0.0.1:<port>/mcp` (default `4123`), not the rmcp SDK. Implements
`initialize` / `tools/list` / `tools/call` / `ping`. Stateless, no session id.
`apply(state)` reconciles the running listener with settings: stops any live
server, starts fresh when `mcp_enabled`. Called at startup and after every
config change, so a toggle restarts the listener rather than mutating it.

**Auth (`mcp/auth.rs`).** Three guards, all mandatory (axum middleware):
1. Bind `127.0.0.1` only, never `0.0.0.0`.
2. `Origin`/`Host` must be loopback when present (DNS-rebinding defense) -> 403.
3. Bearer token on every request, length-checked compare. Token is 256-bit
   random, stored in the OS keychain (`mcp_bearer_token`), never SQLite.

**Tools (`mcp/tools.rs`).** Reads always advertised: `s3_accounts_list`,
`s3_buckets_list`, `s3_objects_list`, `s3_objects_search`, `s3_object_head`,
`s3_bucket_stats`, `s3_transfer_status`. Upload/download gated behind
`mcp_allow_write`, delete behind `mcp_allow_delete`. A disabled capability is
neither advertised in `tools/list` nor dispatched (double gate). Per-tool and
per-account disable sets narrow further. Long ops (upload/download) return a
`transfer_id` to poll, never hold the request open.

**Safety.**
- File tools are confined to `mcp_fs_root`: upload source and download dest are
  canonicalized and must resolve inside that folder, so an untrusted object key
  cannot steer a read/write elsewhere on disk. Unset root = all transfers
  refused. Existing dest paths are fully resolved so a planted symlink cannot
  redirect the write outside root.
- Encrypted buckets refused for data ops (no keychain identity carried into MCP
  v1); delete is allowed (opaque object, no plaintext).
- Delete heads the key first so a missing key reports not-found, not a fake
  success on S3's idempotent DELETE.
- Listings render as compact CSV, capped by a ~100k-char token budget; a budget
  cut suppresses the cursor and flags `has_more` so no rows are silently skipped.

**Background-run.** `app_lifecycle.rs` keeps two atomics, `HAS_ENABLED_WATCH`
and `MCP_ENABLED`; `should_background()` is their OR. The window can close to
tray while either an enabled watch or the MCP server keeps the process alive.
The no-tray Linux caveat is unchanged (close = real quit).

**UI (`routes/Mcp.tsx`, `styles/mcp.css`, `api/mcp.ts`).** Consent gate shown
before enabling, then server/connection/permissions/file-access/accounts/tools
sections. A "Connect a client" picker renders ready-to-paste config per client
with the live endpoint and token (masked until revealed; copy yields the real
value).

---

## Storage Analytics

Per-bucket stats modal (`StatsModal`), computed entirely from the local index
cache - no live bucket calls, so figures reflect the last index sync. Requires
the bucket to be indexed.

- `bucket_stats` command aggregates `cached_objects` via SQL: total size/count,
  size by storage class, top-20 extensions by size (plus a true distinct
  `extension_count`), objects grouped by last-modified month, and the top-10
  largest objects.
- Frontend renders a size-by-type donut (`charts/Donut.tsx`, zero-dep SVG arcs),
  a cumulative growth chart (`charts/TimeSeriesChart.tsx`, uPlot; x = month, y =
  running bytes), per-extension bars, largest-objects list, and storage-class
  breakdown.

---

## Database

SQLite at `{app_data_dir}/cosmog.sqlite`. WAL mode, foreign keys on.

| Module | Tables |
|--------|--------|
| `db/accounts.rs` | Account metadata |
| `db/transfers.rs` | Transfer queue + history |
| `db/cache.rs` | Object metadata cache, FTS5 trigram + BM25 |
| `db/settings.rs` | App settings |
| `db/capabilities.rs` | Cached provider capability probes |
| `db/encryption.rs` | `bucket_encryption` (recipient per bucket) |
| `db/night_watcher.rs` | `nw_watch` (watch config), `nw_file_state` (per-file synced fingerprint) |

Migration rules: append-only to `MIGRATIONS` in `db/mod.rs`. Never edit or reorder.

WAL mode with `busy_timeout=5000` so the Android `:nightwatch` process and the main process can share the file (see Night Watcher below).

---

## PDF Preview

Uses pdfjs-dist v6 legacy build (`pdfjs-dist/legacy/build/pdf.mjs`) loaded lazily via dynamic import. Required because Linux WebKitGTK and Android WebView have no native PDF renderer.

- Bytes fetched via Rust `preview_object` (avoids S3 CORS; handles encrypted buckets)
- Canvas-based rendering: `renderZoom` baked into pixels, `liveScale` transient CSS during pinch, committed on gesture end and re-rendered at full resolution
- Text layer uses `--total-scale-factor` CSS var (pdfjs v6 API)
- Pinch-to-zoom + pan via Pointer Events; Move / Select mode toggle
- Max zoom 4x, canvas pixel cap 6144px - deeper zoom needs windowed rendering (out of scope)
- Drag events suppressed to prevent triggering app's file upload drop handler

---

## Build

```bash
# Desktop dev (hot reload)
npm run tauri dev

# Desktop production
npm run tauri build

# Android debug (arm64)
npm run tauri -- android build --debug --apk --target aarch64

# Android release (all ABIs)
NDK_HOME=$HOME/Android/Sdk/ndk/27.1.12297006 \
ANDROID_HOME=$HOME/Android/Sdk \
npm run tauri -- android build --apk

# Install on device
adb install -r src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk
```

Android prerequisites: Android Studio, SDK 36, NDK 27, Java 17.

```bash
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android i686-linux-android
```

---

## Startup Sequence

1. Resolve `app_data_dir`
2. Check `pending_wipe` marker - if present, wipe and recreate data dir
3. Init tracing (console + rolling log file)
4. Check `cosmog.sqlite.restore_pending`, apply if present
5. Open SQLite (WAL, `busy_timeout`), apply pending migrations
6. Reap orphan transfers (desktop only)
7. Load settings, apply proxy/CA env vars (`apply_network_env`)
8. Prune old request logs
9. Sweep `enc_tmp/`
10. Build `AppState`
11. Spawn background scheduler; spawn Night Watcher loop (desktop) / arm the `:nightwatch` service (Android)
12. Arm desktop background-run (`app_lifecycle`) based on enabled-watch count
13. Reconcile the MCP listener with settings (`mcp::apply`, desktop only)
14. Register commands and run Tauri event loop
