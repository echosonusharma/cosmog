# Cosmog - Agent Guide

Desktop + Android app for managing S3-compatible object storage. Tauri 2, Solid.js frontend, Rust backend.

## Stack

**Frontend:** Solid.js 1.9, TypeScript 5.6, Vite 6, CodeMirror 6, ExcelJS, pdfjs-dist 6 (legacy build), TanStack Solid Virtual  
**Backend:** Tauri 2, Tokio, aws-sdk-s3, rusqlite (SQLite WAL + FTS5), keyring 3, age 0.11 (encryption)

## Architecture

```
Frontend (Solid.js)
    | invoke()
    v
Tauri Commands (src-tauri/src/commands/)
    |
    v
AppState (Arc, cloned into every command)
  +-- TransferManager  ->  ObjectStore trait  ->  S3Store  ->  S3 API
  +-- Db (SQLite)      ->  accounts, transfers, cache, settings, capabilities, encryption
  +-- Secrets          ->  OS Keyring / Android Keystore (never in SQLite)
  +-- crypto           ->  age v1 streaming encrypt/decrypt
```

## Rules

### Code style

- No inline CSS. All styles in `src/styles/`. One CSS file per feature area.
- No giant files. Split when a file grows unwieldy. Prefer small, focused modules.
- One source of truth. No duplicated logic or duplicated type definitions.
- Compartmentalize by feature: browse, transfers, settings, onboarding each own their code.
- All shared TypeScript types go in `src/types/index.ts`. Check there before defining new ones.
- API wrappers in `src/api/` are thin `invoke()` shells only. No logic.
- Optional params passed to `invoke()` use `?? null`, not `undefined`.
- Rust commands use `#[tracing::instrument(skip_all, err)]`.
- Cache writes after remote mutations are best-effort. Never roll back remote ops on cache failure.
- Secrets never touch SQLite. OS keychain only.
- DB migrations: append-only to `MIGRATIONS` in `src-tauri/src/db/mod.rs`. Never edit or reorder.

### Don'ts

- No inline CSS.
- No duplicated logic. Find the existing abstraction before writing a new one.
- Don't scatter feature code across unrelated files. Keep feature logic together.

### Agent behavior

- Check `src/types/index.ts` before defining new types.
- Do not add npm or Cargo dependencies without asking the user first.
- Run `npm run build` (or `tsc --noEmit`) before declaring frontend work done.
- Keep files small. Prefer splitting over appending when a file grows large.
- Test APKs via ADB (`adb install -r <apk>`). Use Android emulator only if no physical device is available.

## Directory Layout

```
src/
  api/           # Tauri invoke() wrappers (one file per domain)
  routes/        # Solid.js pages
    browse/      # Object browser, preview, column nav
    mainapp/     # Shell, sidebar, transfer bar
    onboarding/  # Account setup wizard
    settings/    # Account management
    transfers/   # Transfer queue UI
  state/         # Solid.js signal stores
  styles/        # CSS (no inline styles, keep files small)
  types/         # Shared TypeScript types
  utils/         # Shared helpers

src-tauri/src/
  commands/      # Tauri command handlers (one file per domain)
  db/            # SQLite schema + domain methods
  store/         # ObjectStore trait + S3Store (logging, region retry, TLS)
  transfer/      # TransferManager, worker pool, cancel tokens
  crypto.rs      # age streaming encrypt/decrypt + magic probe
  saf.rs         # Android JNI: SAF upload/download/delete
  scheduler.rs   # Auto-reindex background loop
  secrets.rs     # OS keyring read/write
  state.rs       # AppState
  device.rs      # Android platform detection
```

## Build Commands

```bash
npm run tauri dev              # desktop, hot reload
npm run tauri build            # desktop, production
npm run tauri -- android build --debug --apk --target aarch64
NDK_HOME=$HOME/Android/Sdk/ndk/27.1.12297006 ANDROID_HOME=$HOME/Android/Sdk \
  npm run tauri -- android build --apk   # Android release, all ABIs
```

## Transfer Engine

- Multipart upload with part-level resume on retry
- `CancellationToken` per transfer; orphan transfers reaped at startup
- Encrypted uploads stream through age to `enc_tmp/<uuid>.age`, cleaned after worker settles
- Encrypted downloads probe age magic then stream-decrypt in place
- Events: `Started`, `Progress`, `PartCompleted`, `Done`, `Failed`, `Canceled`

## Client-side Encryption

Per-bucket, transparent. Uses age X25519 + streaming ChaCha20-Poly1305 (64 KiB chunks).

- Secret key (`AGE-SECRET-KEY-...`) stored in OS keychain under `enc:<account_id>:<bucket>`
- Public recipient (`age1...`) stored in SQLite `bucket_encryption` table
- `presign_get` refuses URLs for encrypted buckets unless `allow_ciphertext=true`
- In-memory helpers cap at 512 MiB; preview cap at 128 MiB
- `enc_tmp/` swept unconditionally at startup

## PDF Preview

Uses pdfjs-dist v6 legacy build loaded lazily. Required for Linux WebKitGTK and Android WebView (no native PDF renderer). Bytes fetched via Rust `preview_object` to avoid S3 CORS and support encrypted buckets.

## Startup Sequence

1. Resolve `app_data_dir`
2. Check `pending_wipe` marker - wipe and recreate if present
3. Init tracing (console + rolling log file)
4. Apply `cosmog.sqlite.restore_pending` if present
5. Open SQLite, apply pending migrations
6. Reap orphan transfers
7. Load settings, apply proxy/CA env vars
8. Prune old request logs
9. Sweep `enc_tmp/`
10. Build `AppState`, spawn background scheduler
11. Register commands, run Tauri event loop
