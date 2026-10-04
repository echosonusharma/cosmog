# Cosmog - Agent Guide

Desktop + Android app for managing S3-compatible object storage. Tauri 2, Solid.js frontend, Rust backend.

Architecture, directory layout, transfer engine, encryption, Night Watcher, MCP, startup sequence and scripts are documented in [docs/DOCS.md](docs/DOCS.md). Read it before changing those areas. This file only holds the rules.

## Code rules

- No inline CSS. All styles live in `src/styles/`, one file per feature area.
- Keep files small and focused; split instead of appending to a large file. Group code by feature (browse, transfers, settings, onboarding).
- One source of truth. Find the existing helper or type before writing a new one. No duplicated logic or type definitions.
- Shared TypeScript types go in `src/types/index.ts`.
- `src/api/` wrappers are thin `invoke()` shells with no logic. Optional params use `?? null`, never `undefined`.
- Rust commands use `#[tracing::instrument(skip_all, err)]`.
- Cache writes after remote mutations are best-effort. Never roll back a remote op on cache failure.
- Secrets never touch SQLite. OS keychain / Android Keystore only.
- DB migrations: append-only to `MIGRATIONS` in `src-tauri/src/db/mod.rs`. Never edit or reorder.

## Agent behavior

- Do not add npm or Cargo dependencies without asking the user first.
- Run `npm run build` (or `tsc --noEmit`) before declaring frontend work done.
- Test Android builds on a physical device via `npm run android-install` (builds, installs via adb, launches). Use an emulator only if no device is available.
- Don't run `scripts/release.sh` (bumps, tags and pushes) or `scripts/reset-db.sh` (wipes local data) unless asked.

## Commands

```bash
npm run tauri dev          # desktop, hot reload
npm run tauri build        # desktop, production
npm run android-install    # Android debug build + install (see docs/DOCS.md for flags)
```
