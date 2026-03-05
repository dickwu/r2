# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Cloudflare R2 Client — a cross-platform desktop app for managing S3-compatible storage (Cloudflare R2, AWS S3, MinIO, RustFS). Built with Tauri v2 (Rust backend) + Next.js (React frontend).

## Commands

```bash
bun install              # Install dependencies
bun run dev              # Next.js dev server (port 3000, turbopack)
bun run tauri dev        # Full Tauri desktop app in dev mode
bun run tauri build      # Production build
bun run format           # Prettier format all files
bun run format:check     # Check formatting
```

Rust backend (from `src-tauri/`):
```bash
cargo build              # Build Rust backend only
cargo check              # Type-check Rust code
```

## Architecture

### Frontend (`src/app/`)

- **Framework**: Next.js 16 + React 19, static export (`output: 'export'`) for Tauri
- **UI**: Ant Design 6 + Tailwind CSS 4. Use `App.useApp()` for `message`/`notification`/`modal` — never import static methods from antd
- **State**: Zustand stores in `src/app/stores/` for local UI state; TanStack React Query for server/async state
- **Entry**: `page.tsx` (main UI), `layout.tsx` (providers + global layout)

Key directories:
- `stores/` — Zustand stores: `accountStore`, `syncStore`, `uploadStore`, `downloadStore`, `moveStore`, `batchOperationStore`, `currentPathStore`, `themeStore`, `folderSizeStore`
- `hooks/` — `useFilesSync.ts` (sync orchestration), `useR2Files.ts` (cached file listing)
- `lib/` — `r2cache.ts` (routes sync to provider adapters)
- `components/` — Feature-specific modals and views (ConfigModal, BatchMoveModal, FilePreviewModal, etc.)
- `utils/` — Helpers (`formatBytes`, `fileIcon`)

### Backend (`src-tauri/src/`)

- **Framework**: Tauri v2 with Rust
- **Entry**: `main.rs` → `lib.rs` (plugin registration, IPC commands, window management)
- **Database**: SQLite via Turso (`turso` crate). Schema/queries in `db/`
- **Provider commands**: `commands/r2_commands.rs`, `commands/aws_commands.rs`, `commands/minio_commands.rs`, `commands/rustfs_commands.rs`
- **S3 client**: `providers/s3_client.rs` — shared AWS SDK S3 client factory, provider-specific adapters in `providers/aws/`, `providers/minio/`, `providers/rustfs.rs`
- **File operations**: `upload.rs`, `download/`, `move_transfer/`
- **DB modules**: `db/accounts.rs`, `db/tokens.rs`, `db/buckets.rs`, `db/file_cache.rs`, `db/dir_tree.rs`, `db/downloads.rs`, `db/move_sessions.rs`

### Sync Pipeline

The sync system caches bucket contents locally in SQLite for fast browsing:

1. Frontend: `useFilesSync` → `r2cache.ts` → invokes Tauri command
2. Backend: `sync_*_bucket` command runs 3 phases: **Fetching** (list objects) → **Storing** (write to `cached_files` table) → **Indexing** (build `directory_tree`)
3. Backend emits events: `sync-phase`, `sync-progress`, `indexing-progress`
4. Frontend: `syncStore` receives events, `useR2Files` reads from local cache

### Frontend ↔ Backend Communication

- Tauri IPC via `#[tauri::command]` (Rust) and `@tauri-apps/api` (TypeScript)
- HTTP requests use `@tauri-apps/plugin-http`
- Tauri features are conditionally loaded (check `window.__TAURI__`)
- API responses use `{ code: 1 }` for success

## Key Conventions

- **Package manager**: Bun (not npm/yarn)
- **Formatting**: Prettier with `prettier-plugin-tailwindcss`
- **Naming**: PascalCase for components, camelCase for variables/functions
- **Antd message API**: Always use `const { message } = App.useApp()` — never `import { message } from 'antd'`
- **Provider pattern**: Each storage provider (R2, AWS, MinIO, RustFS) has parallel command, DB, and provider modules — keep them consistent when adding features
- **Accounts are scoped**: Sync data and file cache are per-account + per-bucket, never mixed across accounts
