# Cosmog Project Documentation

Desktop app for managing S3-compatible object storage. Version 0.1.9.

---

## What It Does

Cosmog lets you manage files across S3-compatible object storage from a desktop app. Browse buckets, upload and download files, search, preview content, manage versions, configure multiple accounts, and optionally client-side encrypt entire buckets with per-bucket keys. Credentials and encryption keys are stored securely in the OS keychain.

**Supported providers:** AWS S3, Cloudflare R2, Backblaze B2, DigitalOcean Spaces, Wasabi, MinIO, any S3-compatible API.

---

## Tech Stack

### Frontend

| Tech | Purpose |
|------|---------|
| Solid.js 1.9 | Reactive UI framework |
| TypeScript 5.6 | Type safety |
| Vite 6.0 | Build tool and dev server |
| Tauri 2 | Desktop bridge (Rust to web IPC) |
| CodeMirror 6 | File editor with syntax highlighting |
| TanStack Solid Virtual | Virtualized list rendering |
| ExcelJS | Spreadsheet parsing and preview |
| js-yaml | YAML parsing |
| IBM Plex Sans/Mono | Fonts |

**CodeMirror languages:** JavaScript, TypeScript, JSON, XML, HTML, CSS, Markdown, Python, YAML

### Backend (Rust)

| Crate | Purpose |
|-------|---------|
| tauri 2 | Desktop runtime and command bridge |
| tokio (full) | Async runtime |
| aws-sdk-s3 | S3 API client |
| aws-config | AWS credential and config loading |
| tokio-rusqlite | Async SQLite wrapper |
| rusqlite (bundled) | SQLite with backup API |
| keyring 3 | OS keyring (Apple, Windows, Linux native) |
| serde / serde_json | Serialization |
| tracing + tracing-appender | Structured logging and rolling log files |
| dashmap | Concurrent hashmap |
| thiserror / anyhow | Error handling |
| uuid v4 | ID generation |
| chrono | Timestamps |
| tokio-util | IO utilities |
| futures | Async combinators |
| urlencoding | Key encoding |
| age 0.11 (armor) | Client-side file encryption (X25519 + ChaCha20-Poly1305, streaming AEAD) |
| zeroize | Best-effort scrubbing of key material in memory |

**Tauri plugins:** `tauri-plugin-notification`, `tauri-plugin-opener`, `tauri-plugin-dialog`

---

## Architecture

```
Frontend (Solid.js / TypeScript)
         |  invoke()
         v
Tauri Commands (commands/*)
         |
         v
AppState
  |
  +-- TransferManager -- persistent queue (db/transfers)
  |        |
  |        v
  +-- ObjectStore trait (store/mod)
  |        |
  |        +-- S3Store (store/s3) -- aws-sdk-s3 -- S3 API
  |
  +-- Db (SQLite) -- accounts, transfers, cache, settings, capabilities, bucket_encryption
  |
  |   crypto::{encrypt_file, decrypt_file, encrypt_bytes, decrypt_bytes}
  |         streams age v1 payload during upload/download; magic-probe on read
  |
  Secrets: OS Keyring (S3 credentials + per-bucket encryption identities)
```

**Key design rules:**

- `AppState` is `Arc`-shared across all commands (cheap clone)
- Secrets are never stored in SQLite, only in the OS keyring
- Schema evolves via append-only migrations (never edit or reorder existing entries)
- `ObjectStore` trait is the only abstraction over providers; commands are protocol-agnostic
- Transfer workers emit `TransferEvent` via `ProgressSink` (type-erased, fan-out capable)
- Client-side encryption is per-bucket, transparent to upload/download flows, and enforced only when a config row exists in `bucket_encryption` for that (account, bucket)

---

## Directory Structure

```
cosmog/
+-- src/                          # Frontend (TypeScript/Solid.js)
|   +-- api/                      # Tauri command wrappers
|   |   +-- accounts.ts
|   |   +-- browse.ts
|   |   +-- buckets.ts
|   |   +-- encryption.ts         # enable/disable/export/import bucket key
|   |   +-- logs.ts
|   |   +-- objects.ts
|   |   +-- search.ts
|   |   +-- settings.ts
|   |   +-- transfers.ts
|   +-- routes/                   # Page-level components
|   |   +-- Browse.tsx            # Main file browser
|   |   +-- Logs.tsx              # Diagnostic log viewer
|   |   +-- MainApp.tsx           # Root layout (authenticated)
|   |   +-- Onboarding.tsx        # First-run account setup
|   |   +-- Settings.tsx          # App settings
|   |   +-- Titlebar.tsx          # Custom titlebar
|   |   +-- Transfers.tsx         # Upload/download queue
|   +-- state/                    # Solid.js signal stores
|   +-- types/                    # TypeScript interfaces
|   +-- utils/                    # Reusable components and helpers
|   +-- assets/                   # Static assets
|   +-- App.tsx                   # Root: Onboarding or MainApp
|   +-- main.tsx                  # Entry point
|
+-- src-tauri/                    # Backend (Rust)
    +-- src/
        +-- commands/             # Tauri command handlers
        |   +-- accounts.rs
        |   +-- browse.rs
        |   +-- buckets.rs
        |   +-- bulk.rs
        |   +-- capabilities.rs
        |   +-- encryption.rs     # enable/disable/rotate/export/import per-bucket key
        |   +-- logs.rs
        |   +-- objects.rs
        |   +-- portable.rs
        |   +-- search.rs
        |   +-- settings.rs
        |   +-- transfers.rs
        +-- db/                   # SQLite schema and domain methods
        |   +-- accounts.rs
        |   +-- cache.rs
        |   +-- capabilities.rs
        |   +-- encryption.rs     # bucket_encryption row (recipient) storage
        |   +-- settings.rs
        |   +-- transfers.rs
        +-- store/                # Object store abstraction
        |   +-- mod.rs            # ObjectStore trait
        |   +-- s3.rs             # AWS SDK implementation
        +-- transfer/             # Upload/download engine
        |   +-- manager.rs        # TransferManager and worker pool
        +-- bulk.rs               # Batch ops (folder delete/upload/download)
        +-- crypto.rs             # age v1 streaming encrypt/decrypt + magic probe
        +-- error.rs              # AppError and AppResult
        +-- lib.rs                # App init and command registration
        +-- main.rs               # Tauri entry point
        +-- providers.rs          # Protocol enum and build_store factory
        +-- scheduler.rs          # Background auto-reindex loop
        +-- secrets.rs            # OS keyring read/write
        +-- state.rs              # AppState struct
        +-- sync.rs               # Cache synchronization
        +-- validate.rs           # Account credential validation
```

---

## Tauri API Surface

### Accounts

| Command | Description |
|---------|-------------|
| `add_account` | Insert account and store secret in OS keyring |
| `list_accounts` | All accounts (metadata only, no secrets) |
| `get_account` | Single account by ID |
| `update_account` | Patch fields with optional secret rotation |
| `delete_account` | Cancel in-flight ops, drop row, secret, and cached client |
| `test_account` | Connectivity probe via `list_buckets` |
| `detect_account_region` | Ask bucket for actual region and update if differs |

### Buckets

| Command | Description |
|---------|-------------|
| `list_buckets` | All buckets visible to credentials |
| `create_bucket` | New bucket with optional region pin |
| `delete_bucket` | Remove empty bucket |
| `head_bucket` | Existence and access check |
| `get_bucket_location` | Bucket's actual region |
| `put_bucket_acl` | Set canned ACL (private or public-read) |
| `get_bucket_versioning` | Check if versioning is enabled |
| `put_bucket_versioning` | Toggle versioning on or off |
| `list_multipart_uploads` | In-progress multipart uploads (paged) |
| `cleanup_stale_multiparts` | Abort multiparts older than N seconds |
| `abort_multipart_upload` | Abort one specific upload by ID |

### Objects

| Command | Description |
|---------|-------------|
| `list_objects` | Raw S3 listing (paged, prefix/delimiter) |
| `head_object` | Metadata and refresh local cache |
| `create_folder` | Put zero-byte object with trailing slash |
| `delete_object` | Delete one key and remove cache row |
| `delete_objects` | Batch delete up to 1000 keys |
| `delete_object_version` | Delete specific version (versioned buckets) |
| `list_object_versions` | Versions and delete markers under prefix |
| `copy_object` | Server-side copy with cache write-through |
| `move_object` | Copy then delete with cache mirrored |
| `put_object_acl` | Object-level canned ACL |
| `get_object_tagging` | Read object tag set (AWS only) |
| `put_object_tagging` | Set object tag set (AWS only) |
| `delete_object_tagging` | Clear all tags (AWS only) |
| `presign_get` | Time-limited presigned GET URL |
| `preview_object` | In-memory read of first N bytes for preview |
| `put_object_text` | Save edited text without temp file |
| `put_object_bytes_cmd` | Save binary content (e.g. xlsx) |
| `list_keys_under_prefix` | Live S3 listing, no cache |

### Transfers

| Command | Description |
|---------|-------------|
| `enqueue_upload` | Queue upload; returns transfer_id with events via Channel |
| `enqueue_download` | Queue download; returns transfer_id with events via Channel |
| `list_transfers` | All transfers, optionally filtered by status |
| `get_transfer` | Single transfer row by ID |
| `cancel_transfer` | Signal cancel; idempotent if terminal |
| `retry_transfer` | Re-enqueue with original options and multipart resume |
| `clear_completed_transfers` | Delete done, failed, and canceled rows |
| `clear_transfer` | Delete one transfer row |

### Search and Index

| Command | Description |
|---------|-------------|
| `search_objects` | FTS5 query with facets over local cache |
| `sync_prefix` | Refresh cache for one prefix |
| `bucket_index_status` | Check if full-bucket index is enabled and last sync time |
| `enable_bucket_index` | Turn on indexing and run initial full scan |
| `cancel_bucket_scan` | Stop in-flight full scan (resumable) |
| `reindex_bucket` | Fresh full scan, discard resume token |
| `disable_bucket_index` | Drop cache and turn off indexing |
| `bucket_stats` | Object count, total bytes, and storage class breakdown |
| `set_bucket_auto_reindex` | Schedule re-scan every N seconds |

### Settings

| Command | Description |
|---------|-------------|
| `get_settings` | Load current settings with defaults |
| `update_settings` | Partial-patch update with normalization |
| `reset_settings` | Wipe settings row and return defaults |

### Bulk Operations

| Command | Description |
|---------|-------------|
| `delete_folder_cmd` | Recursive list and batched delete under prefix |
| `upload_directory_cmd` | Walk local dir and enqueue every file |
| `download_directory_cmd` | Walk remote prefix and enqueue every key |
| `cancel_bulk_op` | Cancel in-flight bulk op by op_id |

### Capabilities

| Command | Description |
|---------|-------------|
| `probe_account_capabilities` | Probe `list_buckets` at account level |
| `probe_bucket_capabilities` | Probe head, list, versioning, and location for bucket |
| `get_account_capabilities` | Read cached account-level probe result |
| `get_bucket_capabilities` | Read cached bucket-level probe result |

### Logs

| Command | Description |
|---------|-------------|
| `get_log_dir` | Path to rolling log directory |
| `get_log_tail` | Last N bytes of today's log file |

### Portable (Backup and Restore)

| Command | Description |
|---------|-------------|
| `export_config` | Dump accounts (no secrets) and settings as JSON |
| `import_config` | Merge JSON bundle into local DB |
| `backup_database` | Atomic SQLite backup to path |
| `stage_restore` | Validate and stage SQLite file; applied at next boot |
| `clear_app_data` | Delete all keyring secrets, write wipe marker, and exit; app data dir removed on next boot |

### Browse

| Command | Description |
|---------|-------------|
| `browse_prefix` | Cached children and sub-prefixes with background-refresh if stale |

### Encryption (per-bucket, client-side)

| Command | Description |
|---------|-------------|
| `enable_bucket_encryption` | Generate a fresh X25519 identity, store secret in the OS keychain, record public recipient in DB; requires `confirm_previous_key_saved=true` when `allow_rotate=true` |
| `disable_bucket_encryption` | Remove identity from keychain and config row from DB (existing encrypted objects stay encrypted) |
| `get_bucket_encryption_status` | Return `{ enabled, public_recipient }` |
| `export_encryption_key` | Return `KeyExport` payload (raw bech32 secret + recipient + external decrypt hint) |
| `save_encryption_key_export` | Write the identity to a caller-chosen path with 0600 permissions on Unix |
| `import_encryption_identity` | Load an identity from pasted text; validates parseability + cross-checks recipient against DB |
| `import_encryption_identity_from_file` | Read a saved key file (bounded 64 KiB) then delegate to `import_encryption_identity` |
| `has_encryption_identity` | Preflight for FE: returns `true` iff the keychain has an entry for this bucket |

---

## Transfer System

Transfer engine lives in `src-tauri/src/transfer/manager.rs`.

**Events emitted by workers:**

```
TransferEvent::Started        { transfer_id, bytes_total }
TransferEvent::Progress       { transfer_id, bytes_done, bytes_total }
TransferEvent::PartCompleted  { transfer_id, part_number, etag }
TransferEvent::Done           { transfer_id, etag }
TransferEvent::Failed         { transfer_id, error }
TransferEvent::Canceled       { transfer_id }
```

**Key behaviors:**

- Multipart upload with resume on retry (completed parts stored in DB)
- Configurable concurrency via settings (applied at next launch)
- Orphan transfers left Active or Pending by a crash are reaped at startup
- `CancellationToken` per transfer for cooperative cancel
- `ProgressSink` is type-erased (`Arc<dyn Fn(TransferEvent)>`) and supports fan-out to FE channel and DB simultaneously
- Encrypted buckets: uploads stream through `crypto::encrypt_file` to a temp `.age` file under `<app_data>/enc_tmp/` (cleaned via `opts.cleanup_path` after the worker settles, and swept unconditionally at next startup). Downloads probe age v1 magic on the fetched bytes, then stream through `crypto::decrypt_file` to a sibling `.dec` temp and atomic-rename over the ciphertext. Retries on encrypted buckets never resume from a partial range (age requires the full stream to authenticate).

---

## Client-side Encryption

Optional per-bucket transparent encryption using the [age file format](https://age-encryption.org) (X25519 recipients + streaming ChaCha20-Poly1305 with 64 KiB chunks and last-chunk marker).

**Model:**

- Each encrypted bucket has one age identity. Secret string (`AGE-SECRET-KEY-…`) lives in the OS keychain under `enc:<account_id>:<bucket>`. Public recipient (`age1…`) is stored in the SQLite `bucket_encryption` table.
- Enable/rotate/disable is serialized per `(account, bucket)` via an async mutex in `AppState::encryption_lock` so two concurrent Enable calls cannot generate two identities that overwrite each other.
- Rotate destroys the previous keychain entry irreversibly — the FE must call `enable_bucket_encryption` with both `allow_rotate=true` and `confirm_previous_key_saved=true`, and is expected to walk the user through `export_encryption_key` first.
- All secret-flow strings go through `zeroize::Zeroizing<String>` on the Rust side. The FE clears `freshSecret` on any modal close path. Serialization across the Tauri IPC boundary unavoidably copies; treat the FE-side value as sensitive.

**Upload flow (`commands/transfers.rs::enqueue_upload`):**

1. Look up `bucket_encryption` row for `(account, bucket)`.
2. If present, parse the recipient, stream-encrypt the source file into `<app_data>/enc_tmp/<uuid>.age` via `crypto::encrypt_file` (runs on `spawn_blocking`; constant ~80 KiB RAM).
3. Attach S3 user metadata: `cosmog-encrypted=1`, `cosmog-format=age-v1`, `cosmog-recipient=<age1…>`.
4. Hand off to `TransferManager` with `opts.cleanup_path = tmp_path` (worker deletes it on success or failure).

**Download flow (`transfer/manager.rs`):**

1. Ranged/multipart GET writes bytes to `local_path`. For encrypted buckets, `range_start` is suppressed on retries so the whole authenticated stream is refetched.
2. After the GET succeeds, if a `bucket_encryption` row exists, read the first `AGE_MAGIC.len()` bytes of the downloaded file. If the magic matches (`age-encryption.org/v1\n`), fetch the identity from the keychain and stream-decrypt via `crypto::decrypt_file` to a `.dec` sibling temp, then `tokio::fs::rename` over the ciphertext file.
3. Decrypt failure deletes both `local_path` (was ciphertext) and the `.dec` temp so shell integrations don't index a garbage file at the plaintext filename.
4. Missing keychain entry returns `AppError::EncryptionIdentityMissing` (wire code `encryption_identity_missing`).

**Preview flow (`commands/objects.rs::preview_object`):**

- HEAD → size cap 128 MiB → `read_object_full` → probe age magic on payload bytes (not on S3 metadata, which is attacker-controllable).
- If marker says encrypted but the payload lacks the age header, refuse (`invalid_input: cannot decrypt: payload is not in the expected age format`) rather than serving raw ciphertext.
- Otherwise decrypt in memory via `crypto::decrypt_bytes` and return plaintext bytes plus the original `content_type`.

**Presign refusal:** `presign_get` refuses any URL on an encryption-enabled bucket unless the caller passes `allow_ciphertext=true`, since the recipient of a link would otherwise download ciphertext with no way to open it.

**Export file format:** plain text compatible with `age -i`. First line comment header, then the bech32 secret. Written with `OpenOptions::mode(0o600)` on Unix.

**External decryption:** the exported key file is directly usable with `age -d -i keyfile.txt <ciphertext>` or with the `pyrage` Python library. See the "External decryption guide" section inside `EncryptionModal.tsx` for copy-pasteable snippets.

**Startup sweep:** `<app_data>/enc_tmp/` is unconditionally emptied before `AppState::new` runs. Files there are always ciphertext staged for uploads that never completed and are safe to delete.

**Limits:**

- Streaming file paths: no explicit size cap; bounded only by local disk (source + `.age` temp during encrypt).
- In-memory helpers (`put_object_text`, `put_object_bytes_cmd`): `MAX_INMEMORY_CRYPT_BYTES = 512 MiB`.
- Preview: `MAX_PREVIEW_DECRYPT_BYTES = 128 MiB`.

**Cross-machine access:** the exported identity file is the only durable backup. Keychain contents are not synced between machines. Import via `import_encryption_identity` / `import_encryption_identity_from_file` on the second machine to restore access.

---

## Database

SQLite at `{app_data_dir}/cosmog.sqlite`. WAL mode and foreign keys enabled.

| Module | Tables |
|--------|--------|
| `db/accounts.rs` | Account credentials metadata |
| `db/transfers.rs` | Transfer queue and history |
| `db/cache.rs` | Object metadata cache with FTS5 trigram + BM25 search index |
| `db/settings.rs` | App settings |
| `db/capabilities.rs` | Cached provider capability probe results |
| `db/encryption.rs` | `bucket_encryption` row per encrypted bucket (recipient string; legacy column name `salt_hex`) |

**Migration rules:**

1. Append only to the `MIGRATIONS` array in `db/mod.rs`
2. Never edit or reorder existing entries
3. Keep SQL idempotent where possible (`CREATE IF NOT EXISTS`, `ALTER TABLE`)
4. Version tracked in `schema_migrations` table

**Backup and restore flow:**

- `backup_database`: atomic copy via SQLite Backup API
- `stage_restore`: writes `cosmog.sqlite.restore_pending`
- On next boot: validates SQLite header magic (`SQLite format 3\0`), then renames over live DB

---

## Secrets and Security

- S3 credentials (secret access key) and per-bucket encryption identities are stored in the OS keyring only, never in SQLite
- `keyring` crate uses: Apple Keychain, Windows Credential Manager, Linux Secret Service (GNOME Keyring / KWallet / KeePassXC)
- Keyring reads run via `spawn_blocking` to avoid blocking the Tokio executor
- Config export explicitly excludes secrets
- Per-bucket encryption uses the `age` file format; the encryption engine hand-rolls no crypto
- Exported key files are written with `0600` permissions on Unix
- The Rust side holds secret material in `zeroize::Zeroizing<String>` best-effort; the FE clears the secret signal on every modal close path

---

## Logging

- **Library:** `tracing` + `tracing-subscriber`
- **Outputs:** console (with color) and rolling daily log files
- **Log dir:** `{app_data_dir}/logs/cosmog.log`
- **Level control:** `RUST_LOG` env var; defaults to `info`
- Log guard stored in `OnceLock` to flush queue on clean shutdown
- Frontend can read logs via `get_log_dir` and `get_log_tail` commands

---

## Settings

Configurable options stored in the SQLite `settings` table include:

- `transfer_concurrency` — parallel transfer count (resizable at runtime via `set_transfer_concurrency`)
- `part_size_bytes`, `multipart_parallelism`, `multipart_threshold_bytes` — multipart upload tunables
- `http_proxy` — sets `HTTPS_PROXY` and `HTTP_PROXY` env at startup
- `custom_ca_path` — sets `SSL_CERT_FILE` env at startup (for self-signed certs)
- `request_log_ttl_days` — how long request logs are retained
- `presign_default_expires_secs` — default expiry for `presign_get`

Network env vars are applied once at boot before any SDK client is constructed.

---

## Provider Extensibility

To add a new provider (e.g. Azure Blob):

1. Create `store/<name>.rs` implementing the `ObjectStore` trait
2. Add a variant to the `providers::Protocol` enum
3. Add a branch in `providers::build_store()`
4. The rest of the backend is protocol-agnostic

Current `Protocol` enum: `S3` only. All S3-compatible providers (R2, B2, Spaces, etc.) use the same `S3Store` with different endpoints and configs.

---

## Build and Dev

```bash
# Dev mode (hot reload)
npm run dev

# Production build
npm run build          # tsc + vite build + tauri compile

# Direct Tauri CLI
npm run tauri <cmd>    # uses cross-env NO_STRIP=1
```

**Vite config:** Solid.js plugin, ES2022 target, optimized deps for CodeMirror and ExcelJS.

**Rust test deps:** `tempfile`, `serial_test`, `rand`

**Test surface:** unit + integration tests under `src-tauri/tests/` (buckets, bulk, capabilities, objects, settings, sync/search, transfers, crypto). Crypto tests cover identity roundtrip, encrypt/decrypt bytes, wrong-identity rejection, tamper detection, magic detector, and 5 MiB streaming file roundtrip. S3-touching tests are gated behind a MinIO probe (`require_minio!()`).

---

## App Startup Sequence

1. Resolve `app_data_dir`
2. Check for `pending_wipe` marker; if present, delete entire `app_data_dir` and recreate it
3. Ensure `logs/` exists and init `tracing` (console and rolling file)
4. Check for `cosmog.sqlite.restore_pending`, validate and apply if present
5. Open SQLite and apply pending migrations
6. Reap orphan transfers from crashed session
7. Load settings and apply `http_proxy` and `custom_ca_path` to env
8. Prune request logs older than `request_log_ttl_days`
9. Sweep `<app_data>/enc_tmp/` (leftover encryption temp files from a previous crash)
10. Build `AppState` with configured concurrency semaphore
11. Spawn background scheduler (auto-reindex loop)
12. Register `AppState` with Tauri via `manage()`
13. Register all Tauri commands (~70 including the encryption surface)
14. Run Tauri event loop

---

## Uninstall and Data Removal

App data persists across reinstalls by default. To remove all local data:

**In-app (all platforms):** Settings > Danger zone > Clear all data. Deletes all keyring secrets, writes a `pending_wipe` marker, and exits. On the next launch the entire app data directory is removed before any files are opened.

**Windows uninstaller:** The NSIS uninstaller prompts to delete `%APPDATA%\com.sonus.cosmog`. Note: credentials in Windows Credential Manager are not removed by the NSIS prompt alone; use the in-app option for a complete wipe.

**Manual removal paths:**

| Platform | App data directory |
|----------|--------------------|
| macOS | `~/Library/Application Support/com.sonus.cosmog` |
| Windows | `%APPDATA%\com.sonus.cosmog` |
| Linux | `~/.local/share/com.sonus.cosmog` |

Keyring entries are keyed by account ID under the service name `com.sonus.cosmog`. Per-bucket encryption identities use the entry name `enc:<account_id>:<bucket>` under the same service. On Linux these live in the Secret Service (GNOME Keyring, KWallet, or KeePassXC). On macOS they are in Keychain Access. On Windows they are in Credential Manager.
