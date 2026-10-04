# Cosmog - Developer Docs

Desktop and Android app for managing S3-compatible object storage. Tauri 2, Solid.js frontend, Rust backend. Version lives in `package.json` / `tauri.conf.json`.

---

## Tech Stack

**Frontend:** Solid.js 1.9, TypeScript 7, Vite 8, CodeMirror 6 (editor), Cropper.js (image edit), ExcelJS (spreadsheets), pdfjs-dist 6 (PDF, legacy build), TanStack Solid Virtual, uPlot (charts).

**Backend:** tokio, aws-sdk-s3, rusqlite + tokio-rusqlite (WAL, FTS5), age 0.11 (client-side encryption), blake3 (Night Watcher hashing), keyring 3 (desktop OS keychain), axum 0.8 (local MCP endpoint, desktop only), notify + ignore (desktop file watching), tracing + tracing-appender (rolling logs), jni + ndk-context (Android).

**Tauri plugins:** `dialog`, `fs`, `notification`, `opener`, `store` (UI prefs), and desktop-only `single-instance`, `autostart`, `window-state`.

### Android (Kotlin / JNI)

| Component | Purpose |
| --- | --- |
| `MainActivity`, `CosmogApp`, `NativeBridge` | Entry point, per-process native init (ndk_context + JNI class cache, runs in every process) |
| `TransferService` | Foreground service (dataSync); keeps transfers alive in the background |
| `NightWatchService` | Foreground service in its own `:nightwatch` process; hosts the headless sync loop |
| `BootReceiver`, `NwRestartReceiver` | Re-arm Night Watcher after reboot / after the Android 14+ dataSync timeout |
| `NwTreePicker` | SAF tree picker bridge, polled from Rust |
| `SecretStore` | EncryptedSharedPreferences backed by Android Keystore |

Rust side: `saf.rs` (SAF upload staging, download finalize, tree walk) and `night_watcher_headless.rs`. Any Kotlin class reached from Rust via JNI needs a proguard `-keep` rule (R8 cannot see JNI call sites). The generated Android project is committed under `src-tauri/gen/android/`.

---

## Architecture

```shell
Frontend (Solid.js)
    | invoke()
    v
Tauri Commands (src-tauri/src/commands/)
    |
    v
AppState (Arc, cloned into every command)
  +-- TransferManager  ->  ObjectStore trait  ->  S3Store  ->  S3 API
  +-- Db (SQLite)      ->  accounts, transfers, cache, settings, capabilities, encryption, logs, night watcher
  +-- Secrets          ->  OS Keyring / Android Keystore (never in SQLite)
  +-- crypto           ->  age streaming encrypt/decrypt (upload/download/preview)
```

**Rules**

- Secrets never touch SQLite.
- Schema: append-only `MIGRATIONS` in `db/mod.rs`, never edit or reorder.
- `ObjectStore` is the only provider abstraction; commands are protocol-agnostic.
- Cache writes after remote mutations are best-effort; never roll back a remote op on cache failure.
- All styles in `src/styles/` (no inline CSS). Shared TS types in `src/types/index.ts`. `src/api/` wrappers are thin `invoke()` shells.

## Directory Structure

```shell
src/
  api/        Tauri invoke() wrappers, one file per domain
  routes/     Pages: browse/ (browser, preview, bucketConfig, versionHistory, charts), mainapp/, onboarding/, settings/, transfers/, logs/
  state/      Solid signal stores (app, settings, theme, prefs, toast, confirm)
  utils/      Shared helpers (CodeEditor, icons, fmt, textTypes, errors, ...)
  styles/     CSS, one file per feature area
src-tauri/
  src/commands/   Tauri command handlers, one file per domain (full list: invoke_handler in lib.rs)
  src/db/         SQLite schema + domain methods
  src/store/      ObjectStore trait + S3Store (logging, region retry, Android TLS)
  src/transfer/   TransferManager, worker pool, encrypt
  src/mcp/        Local MCP server (desktop only): mod, auth, tools, format
  src/crypto.rs, secrets.rs, state.rs, scheduler.rs (auto-reindex), app_lifecycle.rs (tray/background-run)
  src/night_watcher.rs, night_watcher_headless.rs, saf.rs, sync.rs, bulk.rs
  tests/          Integration tests (Night Watcher e2e needs --features nw-test-hooks)
```

---

## Transfer Engine

Events: `Started`, `Progress`, `PartCompleted`, `Done`, `Failed`, `Canceled`.

- Multipart upload with part-level resume on retry; `CancellationToken` per transfer.
- Orphan transfers (Active/Pending at crash) are reaped at startup. On Android only `origin = "user"` rows are reaped, since the `:nightwatch` process may hold live rows.
- Encrypted uploads stream through age to `enc_tmp/<uuid>.age`, cleaned after the worker settles. Encrypted downloads probe the age magic then stream-decrypt in place; retries re-fetch the full range (age needs the whole stream to authenticate).
- **Android:** `TransferService` runs while any transfer is active. SAF downloads: the frontend registers `(transfer_id, SAF URI)` before enqueue; after `Done`, `finalize_saf_download` copies the cache file to the URI. On cancel the 0-byte placeholder is removed via `delete_saf_document`.

## Client-side Encryption

Per-bucket and transparent, using the [age format](https://age-encryption.org) (X25519 + streaming ChaCha20-Poly1305, 64 KiB chunks).

- Secret key (`AGE-SECRET-KEY-...`) in the OS keychain under `enc:<account_id>:<bucket>`; public recipient in SQLite `bucket_encryption`.
- Rotating destroys the previous key irreversibly, so the UI makes the user export first. Exported keys work with `age -d -i keyfile.txt`.
- `presign_get` refuses encrypted buckets unless `allow_ciphertext=true`.
- Limits: in-memory helpers cap at 512 MiB, preview at 128 MiB; file streaming is unbounded.

## Night Watcher

One-way background sync of a local directory to an S3 prefix. Local deletes only drop the state row (`delete_policy = "keep"`); remote objects are never deleted.

**Core (`night_watcher.rs`):** a periodic full scan per watch (`full_scan_secs`, default 300) is the source of truth. Change detection is an `mtime + size` fast path; on a miss the file is hashed (blake3). Changed files go through the normal encrypt/enqueue upload path; `nw_file_state` records the fingerprint on completion, `nw_file_retry` backs off failing files. The core is generic over an `NwCtx` trait, so it runs without a `tauri::AppHandle`.

**Desktop:** runs in-process, plus a `notify` watcher as a near-instant accelerator. `app_lifecycle.rs` keeps the process alive after the window closes (tray, close-to-hide, `autostart --hidden`) while an enabled watch or the MCP server exists (`should_background()`). With no system tray, closing the window quits.

**Android:** directories are SAF `content://` trees (no inotify), so it relies on the periodic scan. It runs in `NightWatchService` (`:nightwatch` process), not the main process, because wry calls `process::exit(0)` when the Activity is destroyed.

- Both processes open the same SQLite file (path from JNI `context.getDataDir()`); WAL + `busy_timeout=5000` cover it. Only the `:nightwatch` process runs the loop, so `nw_file_state` has one writer.
- The SAF tree is walked via `DocumentsContract` (JNI in `saf.rs`); changed documents are staged under `<data_dir>/nw_stage/` before upload.
- `START_STICKY` recreates the service after an LMK kill. Android 14+ caps dataSync services at ~6h: `onTimeout` stops cleanly and an exact alarm (`NwRestartReceiver`) re-arms, with the next foreground resume as fallback.

## MCP Server

Desktop only (`cfg(not(target_os = "android"))`), off by default. Lets a local AI client drive S3 ops.

- **One process, one AppState.** Tools call the same state methods as Tauri commands. The listener is a tokio task; `mcp::apply(state)` restarts it on any config change.
- **Transport:** hand-rolled JSON-RPC 2.0 over an axum POST at `127.0.0.1:<port>/mcp` (default 4123). Supports the `2026-07-28` protocol plus the legacy `initialize` handshake, chosen per request.
- **Auth (all mandatory):** bind loopback only; `Origin`/`Host` must be loopback (DNS-rebinding defense); bearer token on every request (256-bit, in the keychain as `mcp_bearer_token`, never SQLite).
- **Tools:** reads always advertised (`s3_accounts_list`, `s3_buckets_list`, `s3_objects_list`, `s3_objects_search`, `s3_object_head`, `s3_bucket_stats`, `s3_transfer_status`). `s3_object_upload` / `s3_object_download` need `mcp_allow_write`; `s3_object_delete` needs `mcp_allow_delete`. A disabled tool is neither advertised nor dispatched. Long ops return a `transfer_id` to poll.
- **Safety:** file tools are confined to `mcp_fs_root` (paths canonicalized, symlinks resolved; unset root refuses all transfers). Encrypted buckets are refused for data ops; delete is allowed. Listings are compact CSV under a ~100k-char budget and set `has_more` instead of silently truncating.

## Storage Analytics

`StatsModal` shows per-bucket stats computed only from the local index cache (no live calls), so the bucket must be indexed. `bucket_stats` aggregates `cached_objects` via SQL (size by class and extension, growth by month, largest objects). Charts: `charts/Donut.tsx` (SVG) and `charts/TimeSeriesChart.tsx` (uPlot).

## Preview and Editors

- **Text types:** `src/utils/textTypes.ts` is the single source for text file types: the "New file" options, extension to MIME lookup, `TEXT_EXTS`, and MIME to editor language. File icons (`utils/icons.tsx`) use the extension first, then the content type.
- **New file:** creates an empty text object (`put_object_text`) with a chosen or custom MIME type, refuses to overwrite an existing key, then opens the editor.
- **Text editor:** CodeMirror via `utils/CodeEditor.tsx`; edits are capped at 10 MiB, preview at 256 KiB.
- **PDF:** pdfjs-dist v6 legacy build, lazily imported (WebKitGTK and Android WebView have no native PDF renderer). Canvas-based with pinch zoom; max zoom 4x.
- **Images:** Cropper.js, re-encoded via canvas to png/jpeg/webp. Overwrite is blocked when the output format differs from the source extension; save-as writes `name-edited.<ext>`. Source cap 30 MiB (`EDITOR_MAX_BYTES`).
- All preview and edit bytes come through Rust `preview_object` / `put_object_*`, which avoids S3 CORS and handles encrypted buckets.

## Database

SQLite at `{app_data_dir}/cosmog.sqlite`, WAL mode, foreign keys on. Tables: `accounts`, `transfers`, `bucket_index`, `cached_objects` (+ FTS5 trigram `cached_objects_fts`), `prefix_sync`, `settings`, `account_capabilities`, `bucket_capabilities`, `request_logs`, `bucket_encryption`, `nw_watch`, `nw_file_state`, `nw_file_retry`. Domain methods live in `src-tauri/src/db/`.

## Frontend UI Prefs

UI-only state (view mode, last location) persists via `@tauri-apps/plugin-store` (`prefs.json`), separate from the Rust settings DB. `state/prefs.ts` loads it once at boot into a sync cache so signals can read at creation time. Window size/position use `tauri-plugin-window-state`.

---

## Build

```bash
npm run tauri dev                                           # desktop, hot reload
npm run tauri build                                         # desktop, production
npm run tauri -- android build --debug --apk --target aarch64   # Android debug (arm64)

# Android release (all ABIs)
NDK_HOME=$HOME/Android/Sdk/ndk/27.1.12297006 ANDROID_HOME=$HOME/Android/Sdk \
  npm run tauri -- android build --apk

adb install -r src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk
```

Android prerequisites: Android Studio, SDK 36, NDK 27, Java 17, and the Rust targets:

```bash
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android i686-linux-android
```

Rust tests: start MinIO with `docker compose up -d minio` (`docker-compose.yml`, `localhost:9000`, override with `MINIO_ENDPOINT`), then `cargo test` in `src-tauri/`. Without MinIO the S3 suites skip silently and still pass. The Night Watcher e2e also needs `--features nw-test-hooks --test night_watcher_e2e`.

## Scripts

All in `scripts/`, run from anywhere (they resolve the repo root).

| Script | npm alias | Purpose |
| --- | --- | --- |
| `android-install.sh` | `npm run android-install` | Build the APK and install it on every connected adb device, then launch the app |
| `release.sh <vX.Y.Z> [semver]` | `npm run release -- v1.2.3` | Bump the version, commit, tag, push |
| `reset-db.sh` | `npm run reset-db` | Delete the local SQLite DB (Linux path `~/.local/share/com.sonus.cosmog`) |

**`android-install.sh`:** debug build by default, compiled only for the ABIs of connected devices (falls back to arm64 with no device). It installs the smallest matching split APK per device, retrying a flaky install up to 3 times. Flags: `--release` (all ABIs), `--abi <abi>` (override detection), `--universal` (single large APK), `--no-launch`. Defaults `JAVA_HOME`, `ANDROID_HOME` and `NDK_HOME` (NDK 27.1) if unset. It deletes `gen/android/app/build/outputs/apk` first, because Gradle's incremental packaging can leave APKs padded with hundreds of MB of zeros.

**`release.sh`:** the tag must be `vX.Y.Z`; the version defaults to the tag without the `v`. It rewrites the version in `package.json`, `tauri.conf.json` and `Cargo.toml`, refreshes `Cargo.lock`, commits `chore: bump version to <tag>`, tags, and pushes the branch and tag. Pushing a `v*` tag triggers `.github/workflows/release.yml` (desktop + Android builds, GitHub release). Only run it from a clean, up-to-date branch.

**`reset-db.sh`:** removes `cosmog.sqlite` and its `-wal`/`-shm` files. Accounts, transfers, cache and settings are lost; keychain entries (secrets, encryption keys) are not touched. Quit the app first.

---

## Startup Sequence

1. Resolve `app_data_dir`; if a `pending_wipe` marker exists, wipe and recreate it.
2. Init tracing (console + rolling log file).
3. Apply `cosmog.sqlite.restore_pending` if present.
4. Open SQLite, apply pending migrations.
5. Reap orphan transfers (all on desktop, `origin = "user"` only on Android).
6. Load settings, apply proxy/CA env vars.
7. Build `AppState`.
8. Background work: prune old request logs, sweep `enc_tmp/`, start the scheduler, Night Watcher (desktop in-process, Android via the `:nightwatch` service), desktop background-run, and the MCP listener (desktop).
9. Register commands, run the Tauri event loop.
