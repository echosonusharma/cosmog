<div align="center">
  <img src="src-tauri/icons/cosmog-icon.svg" width="96" height="96" alt="Cosmog" />

  # Cosmog

  Native desktop app for managing S3-compatible object storage.  
  Browse, upload, download, and organize files across any S3 provider.
</div>

## Features

- **Browse** buckets and objects with folder navigation, column layout, and search
- **Upload & download** files with background transfer queue, progress tracking, and retry
- **Preview** images, text, JSON, XML, and spreadsheets inline
- **Edit** text files directly in the app
- **Bulk operations** — multi-select delete, copy presigned links
- **Create & delete** buckets, folders, and objects
- **Copy / move** objects within and across buckets
- **Presigned URLs** — generate shareable links with custom expiry
- **Versioning** — view and toggle bucket versioning
- **Full-text search** with local index per bucket
- **Multiple accounts** — manage many credentials side by side
- **Transfer manager** — real-time speed, filter by active/done/failed
- **Request logs** — searchable history of every S3 API call with operation/status filters, configurable retention
- **System logs** — live-tailing log viewer with level filter and search
- **Multi-region aware** — buckets are routed to their own AWS region automatically, no manual region config
- **Secure credentials** — secrets live in the OS keychain (Keychain, Credential Manager, Secret Service), never on disk
- **Backup & restore** — export/import accounts and settings as JSON (secrets excluded)
- **Themes** — light, dark, or follow system

## Supported Providers

| Provider | Notes |
|---|---|
| Amazon S3 | Native AWS |
| Cloudflare R2 | Custom endpoint required |
| Backblaze B2 | Custom endpoint required |
| DigitalOcean Spaces | Custom endpoint required |
| Wasabi | Custom endpoint required |
| MinIO | Self-hosted |
| S3-compatible | Any S3-compatible API |

## Download

| Platform | |
|---|---|
| macOS (Apple Silicon) | [Download](https://github.com/echosonusharma/cosmog/releases/latest) |
| macOS (Intel) | [Download](https://github.com/echosonusharma/cosmog/releases/latest) |
| Windows | [Download](https://github.com/echosonusharma/cosmog/releases/latest) |
| Linux (AppImage) | [Download](https://github.com/echosonusharma/cosmog/releases/latest) |
| Linux (deb) | [Download](https://github.com/echosonusharma/cosmog/releases/latest) |

> Credentials are stored in the native OS secret store: Keychain on macOS,
> Credential Manager on Windows, and the D-Bus Secret Service on Linux —
> where a provider such as GNOME Keyring, KWallet, or KeePassXC must be running.

## Development

```sh
npm install
npm run tauri dev     # run the app with hot reload
npm run tauri build   # production bundles
```

Architecture, module layout, and internals are documented in [DOCS.md](DOCS.md).
