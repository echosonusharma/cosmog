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
| `NwTreePicker` | SAF tree picker bridge, polled from Rust; shared by Night Watcher and folder upload (`fu_pick_tree`), picks serialized by a Rust mutex |
| `SecretStore` | EncryptedSharedPreferences backed by Android Keystore |

Rust side: `saf/` (`read` upload staging + hashing, `write` download finalize, `walk` streaming tree walk (one query per dir, visitor decides descent), `tree` Night Watcher file list on top of it, `fd` (`open_saf_fd`: `openFileDescriptor` + `detachFd`, regular files only), `uri` (tree URI normalization, document URIs, display names), `picker`, `service`) and `night_watcher_headless.rs`. `/proc/self/fd/N` cannot be reopened (FUSE EACCES), so every open of a document goes through ContentResolver; `transfer/local_source.rs` routes `content://` sources there for the whole upload pipeline (stat, single/multipart bodies, encryption). `TransferService` is held by the UI (`set_transfer_service`) OR a running folder job; a UI `false` never stops it while a folder runner is live. Any Kotlin class reached from Rust via JNI needs a proguard `-keep` rule (R8 cannot see JNI call sites). The generated Android project is committed under `src-tauri/gen/android/`.

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
  +-- Db (SQLite)      ->  accounts, transfers, cache, settings, capabilities, encryption, logs, night watcher, folder uploads
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
  routes/     Pages: browse/ (browser, preview, bucketConfig, versionHistory, charts, folderUpload), mainapp/, onboarding/, settings/, transfers/ (incl. FolderJobRow, FolderJobDetails), logs/
  state/      Solid signal stores (app, settings, theme, prefs, toast, confirm, folderJobs)
  utils/      Shared helpers (CodeEditor, icons, fmt, textTypes, errors, ...)
  i18n/       t()/tp(), device language detection, flags, locales/<lang>/<area>.ts
  styles/     CSS, one file per feature area
src-tauri/
  src/commands/   Tauri command handlers, one file per domain (full list: invoke_handler in lib.rs)
  src/db/         SQLite schema + domain methods (cache/: objects, index, browse, search, facets, stats; folder_upload/: jobs, items)
  src/store/      ObjectStore trait + S3Store (s3/: download, upload, copy, classify), logging, region retry, Android TLS
  src/transfer/   TransferManager (manager), worker, sink (progress + parts journal), semaphore, encrypt
  src/mcp/        Local MCP server (desktop only): mod, auth, tools, format
  src/folder_upload/  Folder upload: walk (+ walk_saf), analyze, source (path/SAF branch, grants), remote (check), engine + item (runner), control (scheduler), resume, keys, policy
  src/crypto.rs, secrets.rs, state.rs, scheduler.rs (auto-reindex), app_lifecycle.rs (tray/background-run)
  src/night_watcher/ (scan, reconcile, upload, fs, desktop), night_watcher_headless.rs, saf/, sync.rs, bulk.rs
  tests/          Integration tests (Night Watcher e2e needs --features nw-test-hooks)
```

---

## Transfer Engine

Events: `Started`, `Progress`, `PartCompleted`, `Done`, `Failed`, `Canceled`.

- Multipart upload with part-level resume on retry; `CancellationToken` per transfer.
- Part size (`adaptive_part_size`, `store/s3/upload.rs`): the configured size (5 MiB floor, 5 GiB cap) when the file fits in 10,000 parts, else the smallest whole-MiB size that does.
- Resume: retrying a row that holds an `upload_id` rebuilds the completed parts from a full paginated `ListParts`, dropping parts whose size does not fit the layout (they are re-sent). The parts journal is sparse (flushed at power-of-two part counts), so it is only the fallback when `ListParts` is `unsupported` or `access_denied`. `NoSuchUpload`, a source whose len+mtime changed, or a saved layout past S3 limits (part over 5 GiB or over 10,000 parts) starts fresh. Encrypted retries always abort and restart (new ciphertext).
- **upload_id:** a new id is written to the row before the first part is sent. When a row ends without completing, it drops the id only after the abort succeeded or returned `NoSuchUpload`; otherwise the row keeps it for a later cleanup.
- **Conditional writes:** `PutOptions.if_none_match` is sent on PutObject and CompleteMultipartUpload. A provider that rejects the header gets HEAD then write instead, remembered per account store (`inm_unsupported`) for later calls; this leaves a short race, and a provider that silently ignores the header cannot be detected. A bare 412 is `conflict`. Only folder jobs set it.
- **Source guard** (`transfer/source_guard.rs`, opt-in, folder jobs only): the opened file must be the regular file seen at enqueue (device+inode on unix, size+mtime on Windows, where std has no stable file index) and resolve inside the canonical root, re-checked without following links. Guarded bodies stream from the opened handle, so the SDK cannot retry them itself. std has no `O_NOFOLLOW`, so two link swaps within microseconds (file, or a parent folder) can still slip through.
- `suspend(id)` stops a live worker without aborting its multipart upload: the row ends `paused` with the journal flushed and the sink gets `Canceled`. `retry` accepts `paused` rows.
- Job-owned rows: `enqueue_upload_for_job` sets `transfers.job_id`, and `retry` carries it over. A network-class failure (`network_unreachable`, `dns_failure`, `connection_refused`, `request_timeout`) leaves such an upload row `paused` with its multipart upload for the job to resume; other rows end failed as before. `list_transfers`, clear completed, `prune_finished_transfers` and `list_finished_with_upload` skip these rows; the owning folder job manages them.
- Orphan transfers (Active/Pending at crash) are reaped at startup. On Android only `origin = "user"` rows are reaped, since the `:nightwatch` process may hold live rows.
- Encrypted uploads stream through age to `enc_tmp/<uuid>.age`, cleaned after the worker settles. Encrypted downloads probe the age magic then stream-decrypt in place; retries re-fetch the full range (age needs the whole stream to authenticate).
- **Android:** `TransferService` runs while any transfer is active. SAF downloads: the frontend registers `(transfer_id, SAF URI)` before enqueue; after `Done`, `finalize_saf_download` copies the cache file to the URI. On cancel the 0-byte placeholder is removed via `delete_saf_document`.

## Folder Upload

Desktop folders and Android SAF trees (`source_kind` `path` or `saf`); the backend (`folder_upload/`, `commands/folder_upload.rs`) compiles everywhere. MCP has no folder upload. Persistence is `folder_upload_jobs` (one row per job) plus `folder_upload_items` (one row per walked entry).

**Lifecycle:** analyze (walk the local tree into items) -> remote check (live listing of the destination) -> approve -> run -> finish. Job states: `analyzing`, `analyzed`, `queued`, `running`, `paused`, `completed`, `completed_with_errors`, `canceled`, `failed`. `completed_with_errors` means some items failed, were `skipped_missing`, were canceled, or became un-uploadable after approval. `failed` is defined but never produced; `retry_failed` rejects it. `source_root` is stored canonical (links resolved, no `\\?\` prefix on Windows); the default destination name comes from the picked folder. Only one unfinished job may target the same source and destination (`folder_job_exists`).

- **Manifest:** the item list is what gets uploaded. Files added to the folder after analysis are not uploaded.
- **Keys:** `dest_prefix + rel_path`, joined with `/`. Names are never trimmed, rewritten or lossy-converted. Names Windows cannot create are uploaded and only counted in the analysis.
- **Excluded** (`exclude_reason`, listed in the analysis, never block the job): `symlink` (never followed; the target is shown), `special` (FIFOs, sockets, devices, never opened), `unreadable` / `unreadable_dir`, `non_utf8`, `control_chars` (U+0000-001F, U+007F break XML listings), `key_too_long` (over 1024 UTF-8 bytes, re-evaluated when the destination changes), `file_too_large` (over 5 TiB). Only the picked root itself may be a link. A folder whose listing errors partway gets an `unreadable_dir` item; for the root its `rel_path` is `""`.
- **Hidden files** (leading `.` or the Windows hidden attribute) are uploaded; the analysis reports them separately.
- **Empty directories** (or folders whose children are all excluded) become a `rel/` marker object when `create_dir_markers` is set (default on). Other directories get no marker. Markers are conditional and never replace an existing `rel/`.
- **Overwrite check:** a segmented live listing: the destination with delimiter `/`, then a full listing only of the local top-level dirs that exist remotely. A remote `x/` marker is not an overwrite. A destination change cancels the running check. A listing that says truncated without a continuation token fails the check (`s3`) and approval is refused. Approve requires the check done for the current destination, and an overwrite policy (`skip` or `overwrite`, no default) when anything exists.
- **Late keys:** a missing policy behaves as `skip`, and the frontend always sends `skip` when nothing exists. Under `skip` every file write is conditional (see Conditional writes); a 412 marks the item `skipped_exists` unless the key holds the job's own write. The job replaces only objects it wrote itself, identified by `x-amz-meta-cosmog-fu = <job>/<seq>/<size>/<mtime>` or the stored ETag (`done_etag`), checked by HEAD just before (short race). Only `overwrite` replaces foreign objects.
- **Window:** at most W = min(2 x `transfer_concurrency`, 32) job transfer rows exist at once, re-read on each refill. A row is deleted as soon as its item settles.
- **Scheduling:** one job runs at a time; the next is the running job, else the oldest `queued`.
- **Drift:** before upload each file is re-stat'ed: gone means `skipped_missing`; now a link, special file, outside the root or over 5 TiB means `excluded` with `drift = changed` (ends the job `completed_with_errors`); changed size/mtime uploads the current content (`drift = changed`). A file that changes during its upload is re-uploaded once, then fails with `source_changed`, as does one that stops being a regular file inside the folder right after its upload.
- **Retries:** network errors (`network_unreachable`, `dns_failure`, `connection_refused`, `request_timeout`) park the item with its transfer row and multipart upload, and the job waits (`waiting = network`), probing `head_bucket` with a 10 s to 5 min backoff, then resumes the same upload. A failure counts only when the probe passed and the file made no progress; the 5th counted failure fails the item, so a link that drops while parts still get through never fails it. Auth, quota, bucket and TLS errors (`access_denied`, `credentials_*`, `token_expired`, `clock_skew`, `quota_exceeded`, `bucket_not_found`, `invalid_region`, `tls_error`) pause the job with the code as `pause_reason`. `rate_limited`, `service_unavailable`, `s3` back off per item 30 s doubling (cap 30 min), 5 attempts; `io` gets 3 attempts 30 s apart; anything else fails the item. `retry_failed` resets failed items and reopens a finished job.
- **Preflight:** a missing source root, or one that now resolves elsewhere, pauses with `folder_source_unavailable` (detail holds only the folder name). Turning bucket encryption on or off pauses with `folder_encryption_changed`, and resume continues under the current setting; a key rotation just switches the recipient.
- **Pause** `suspend`s in-flight transfers, so plain multipart uploads keep their parts. **Cancel** of an unapproved job deletes it; of an approved one ends `canceled`: pending and in-flight items end canceled, multipart uploads are aborted, rows deleted. Objects already uploaded stay in the bucket.
- **Resume:** `running` and `queued` jobs continue at startup; `paused` stays paused. Each in-flight item is matched to its row: a `done` row finishes the item, otherwise the row is retried. Done items are never re-uploaded; an `uploading` item whose row is gone is marked done only if a HEAD finds this job's upload of the current version, else it goes back to pending. A plain multipart upload continues under the same `upload_id` without re-sending listed parts; an encrypted file restarts from scratch. Rows no item points at are aborted and deleted.
- **Counts:** per-state item counts live in `folder_upload_counts`, kept by triggers on `folder_upload_items`, so progress never scans the items.
- **Limits:** 500,000 walked entries per job (hard, `folder_too_many_files`). Soft warnings: over 10,000 files, over 50 GiB, hidden files present, exclusions present, encrypted (temp space).
- **History:** finished jobs stay until removed; removing aborts and deletes any rows they still hold. Startup keeps the newest 50 finished jobs and drops those older than 30 days. Deleting an account or bucket cancels its jobs first.
- **Android (SAF):** `fu_pick_tree` returns `{ uri, display_name }` (uri normalized, persisted READ grant), passed as `local_root` to analyze. The job stores the tree URI as `source_root`; each file item stores its document URI in `source_ref`, which is also the transfer row's `local_path` so retries reopen it via JNI. The walk (`walk_saf.rs`) applies the desktop rules plus: virtual documents are `special`; names that are empty, `.`/`..`, contain `/` or repeat a sibling are `invalid_name`; a missing size falls back to opening the document, else `unreadable`. No source guard (no inode identity or links); drift (before upload, after upload, and on restart) compares size, plus mtime only for local storage (`com.android.externalstorage.documents`, where listing and fstat agree; 0 = unknown); other providers are size-only since they may serve cache copies. Preflight queries the tree root; a lost grant (preflight, per-file stat, or open during upload) pauses with `folder_source_unavailable`. On Android the job's Resume re-picks first (must be the same tree), and picking that tree for a new upload resumes the waiting jobs instead. The persisted grant is released (matched against `getPersistedUriPermissions` by normalized tree) only when no job holds it (unfinished or `completed_with_errors`), no picker is open, no pick awaits analyze or `nw_add_watch` (30 min), and no Night Watcher watch uses it; checked under the scheduler lock at job end, cancel, remove, on analyze refusing a pick, on `fu_release_pick` (re-pick of a different folder), and at startup for every persisted tree grant. Analyze signals idle before it takes that lock (cancel holds it while waiting). `FolderJob.display_name` / `FolderAnalysis.display_name` give the folder name for both kinds.
- **Frontend:** the backend emits `folder-upload-changed` (`{ job_id, state }`). `state/folderJobs.ts` is the single shared poller (1 s while a job moves, 5 s idle, plus a refresh on each event); the Transfers page renders one `FolderJobRow` per job and its "Clear completed" also clears finished jobs. While a job moves, closing the window asks first if it would really quit (`app_close_quits`).

## Client-side Encryption

Per-bucket and transparent, using the [age format](https://age-encryption.org) (X25519 + streaming ChaCha20-Poly1305, 64 KiB chunks).

- Secret key (`AGE-SECRET-KEY-...`) in the OS keychain under `enc:<account_id>:<bucket>`; public recipient in SQLite `bucket_encryption`.
- Rotating destroys the previous key irreversibly, so the UI makes the user export first. Exported keys work with `age -d -i keyfile.txt`.
- `presign_get` refuses encrypted buckets unless `allow_ciphertext=true`.
- Limits: in-memory helpers cap at 512 MiB, preview at 128 MiB; file streaming is unbounded.

## Night Watcher

One-way background sync of a local directory to an S3 prefix. Local deletes only drop the state row (`delete_policy = "keep"`); remote objects are never deleted.

**Core (`night_watcher/`):** a periodic full scan per watch (`full_scan_secs`, default 300) is the source of truth. Change detection is an `mtime + size` fast path; on a miss the file is hashed (blake3). Changed files go through the normal encrypt/enqueue upload path; `nw_file_state` records the fingerprint and the bucket's encryption recipient on completion, `nw_file_retry` backs off failing files. The core is generic over an `NwCtx` trait, so it runs without a `tauri::AppHandle`.

**Encryption changes:** a state row only counts as synced under the bucket's current recipient, so enabling, rotating or disabling bucket encryption re-uploads already-synced files. On desktop the encryption commands force a scan of that bucket's watches on the next tick; Android waits for the next periodic scan. After upgrading past migration 20, watches on encrypted buckets re-upload everything once.

**Desktop:** runs in-process, plus a `notify` watcher as a near-instant accelerator. While a watch's watcher is healthy its full scan stretches to at least 1h (`WATCHED_SCAN_SECS`); a new watcher, a notify error or an overflow (`Flag::Rescan`) forces a scan on the next tick, and an error drops the watch back to `full_scan_secs`. A directory event walks that subtree, since a moved-in dir can arrive as one event. Open/read access events are dropped (only close-after-write counts), so hashing a file never retriggers it. `app_lifecycle.rs` keeps the process alive after the window closes (tray, close-to-hide, `autostart --hidden`) while an enabled watch or the MCP server exists (`should_background()`). With no system tray, closing the window quits.

**Android:** directories are SAF `content://` trees (no inotify), so it relies on the periodic scan. It runs in `NightWatchService` (`:nightwatch` process), not the main process, because wry calls `process::exit(0)` when the Activity is destroyed.

- Both processes open the same SQLite file (path from JNI `context.getDataDir()`); WAL + `busy_timeout=5000` cover it. Only the `:nightwatch` process runs the loop, so `nw_file_state` has one writer.
- The SAF tree is walked via `DocumentsContract` (JNI in `saf/tree.rs`); changed documents are staged under `<data_dir>/nw_stage/` before upload.
- `START_STICKY` recreates the service after an LMK kill. Android 14+ caps dataSync services at ~6h: `onTimeout` stops cleanly and an exact alarm (`NwRestartReceiver`) re-arms, with the next foreground resume as fallback.
- Reboot resume: `BOOT_COMPLETED` is an FGS-start exemption up to Android 14, so `BootReceiver` starts the service directly, no permission needed. Android 15+ forbids a boot receiver from starting a `dataSync` FGS, so it arms a 15s alarm and `NwRestartReceiver` does the start, an alarm being an exemption. That hop and the hourly timeout restart both need `SCHEDULE_EXACT_ALARM`, which is not pre-granted to fresh installs targeting Android 13+: the Night Watcher page prompts for it (`nw_can_schedule_exact_alarms` / `nw_request_exact_alarms`) when it is missing, and without it a restart waits for the next app open. `nw_boot_pending` tracks that, set by the receiver and cleared by the service only once its sync loop is running, so a refused start keeps the fallback armed.

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

SQLite at `{app_data_dir}/cosmog.sqlite`, WAL mode, foreign keys on. Tables: `accounts`, `transfers`, `bucket_index`, `cached_objects` (+ FTS5 trigram `cached_objects_fts`), `prefix_sync`, `settings`, `account_capabilities`, `bucket_capabilities`, `request_logs`, `bucket_encryption`, `nw_watch`, `nw_file_state`, `nw_file_retry`, `folder_upload_jobs`, `folder_upload_items`, `folder_upload_counts`. `transfers.job_id` (migration 21) marks rows owned by a folder job. Domain methods live in `src-tauri/src/db/`.

## i18n

English, Chinese (zh), Japanese (ja), Spanish (es), Russian (ru), Korean (ko), Brazilian Portuguese (pt). `src/i18n/`: `index.ts` (reactive `t()` / `tp()` for plurals, `initI18n`, `setLanguage`), `resolve.ts` (device language matching), `flags.ts` + `flags/*.svg` (SVG, not emoji, so Windows renders them), `locales/<lang>/<area>.ts` (one flat `"area.key": "text"` file per area).

- The language pref is `system` (default) or a language code, stored in `prefs.json`. `system` picks the first device language we ship (`zh-*` maps to `zh`), else English. Missing keys fall back to English per key.
- Add a string: put it in `locales/en/<area>.ts` first (the key type comes from the English files), then in every other language. Reuse `common.*` for generic words. Never build text at module level or by joining fragments; use placeholders `{name}` and `tp()` for plurals.
- Plurals: English, es and pt need `_one` and `_other`; ru needs `_one`, `_few`, `_many`, `_other`; zh, ja and ko only `_other`. A missing key falls back to English.
- Keep `{placeholders}` identical in every language.
- Add a language: register it in `languages.ts` and `flags.ts` (+ SVG), add its loader in `index.ts`, and copy the empty locale files.

## Errors

The backend never sends user-facing text. Commands fail with `AppError`, serialized as a JSON string `{ "code": "...", "detail": "..." }`. `code` is a stable snake_case contract (`src-tauri/src/error.rs`, one row per code in `app_errors!`); `detail` is the raw technical reason (SDK message, OS error) and is only shown behind "Technical details", in logs and request logs.

- S3 failures are classified in `store/s3/classify.rs`: S3 error code first, HTTP status second, connection errors by cause (DNS, refused, TLS, timeout). Branch in Rust with `e.code()` or the `is_not_found()` / `is_retriable()` helpers, never on text.
- Failed transfers and watcher scans persist `AppError::to_wire_string()`, so the UI can translate them later. Older rows with plain text still parse (`utils/wireError.ts`).
- The FE title and description per code live in `locales/<lang>/errors.ts` as `errors.<code>.title` / `.desc`. `utils/errors.ts` (`describeError`, `errMsg`, `errCode`) renders them; `ErrorBody` is the shared card with the code and details.
- Add a code: a row in `app_errors!`, the code in `src/utils/errorCodes.ts`, and the title and desc in every language's `errors.ts`. `errorCodes.ts` must mirror `AppError::CODES`; tests in `error.rs` fail if the list or a translation is missing.

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

Rust tests: start MinIO with `docker compose up -d minio` (`docker-compose.yml`, `localhost:9000`, override with `MINIO_ENDPOINT`), then `cargo test` in `src-tauri/`. Without MinIO the S3 suites skip silently and still pass. The Night Watcher e2e also needs `--features nw-test-hooks --test night_watcher_e2e`. Folder upload: `folder_upload_db` (SQLite only), `folder_upload_engine` (in-memory store), `folder_upload` (MinIO); `FU_REQUIRE_MINIO=1` makes `folder_upload` fail instead of skip when MinIO is down.

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
8. Background work: prune old request logs, sweep `enc_tmp/`, folder jobs (`folder_upload::startup`: delete unapproved jobs, purge finished jobs' rows, prune history, sweep job rows whose job is gone, resume running/queued jobs), start the scheduler, Night Watcher (desktop in-process, Android via the `:nightwatch` service), desktop background-run, and the MCP listener (desktop).
9. Register commands, run the Tauri event loop.
