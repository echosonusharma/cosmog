# Cosmog - Developer Docs

Desktop and Android app for managing S3-compatible object storage. v0.1.13.

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

### Backend (Rust)

| Crate | Purpose |
|-------|---------|
| tauri 2 | App runtime and command bridge |
| tokio | Async runtime |
| aws-sdk-s3 / aws-config | S3 API |
| tokio-rusqlite / rusqlite | SQLite (WAL mode, FTS5) |
| keyring 3 | OS keychain (Apple / Windows / Linux Secret Service) |
| age 0.11 | Client-side encryption (X25519 + ChaCha20-Poly1305 streaming) |
| zeroize | Best-effort scrub of key material |
| serde / serde_json | Serialization |
| tracing + tracing-appender | Structured logging, rolling files |
| thiserror / anyhow | Error handling |

**Tauri plugins:** `dialog`, `fs`, `notification`, `opener`

### Android (Kotlin / JNI)

| Component | Purpose |
|-----------|---------|
| `MainActivity.kt` | Tauri entry point |
| `TransferService.kt` | Foreground service (dataSync); keeps transfers alive when backgrounded |
| `SecretStore.kt` | EncryptedSharedPreferences backed by Android Keystore |
| `saf.rs` | JNI bridge for Storage Access Framework (upload staging, download finalize, delete placeholder) |

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
|   |   |   +-- ObjectBrowser, PreviewPane, ColumnPane, ListView, ...
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
    |   +-- saf.rs              # Android JNI: SAF upload/download/delete
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
- Orphan transfers (Active/Pending at crash) reaped at startup
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

Migration rules: append-only to `MIGRATIONS` in `db/mod.rs`. Never edit or reorder.

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
5. Open SQLite, apply pending migrations
6. Reap orphan transfers
7. Load settings, apply proxy/CA env vars
8. Prune old request logs
9. Sweep `enc_tmp/`
10. Build `AppState`
11. Spawn background scheduler
12. Register commands and run Tauri event loop
