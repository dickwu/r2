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
bun run check            # format:check + typecheck + bun test — the frontend CI gate
bun test                 # Frontend unit tests (Bun's runner; no vitest/jest)
bun run typecheck        # tsc --noEmit
bun run format           # Prettier format all files
bun run format:check     # Check formatting
```

Rust backend (from `src-tauri/`):

```bash
cargo build              # Build Rust backend only
cargo check              # Type-check Rust code
cargo test --workspace   # Rust tests (root crate + crates/range-dl)
cargo fmt --all --check                                # CI gate
cargo clippy --workspace --all-targets -- -D warnings  # CI gate
```

## CI & Releases

- CI (`.github/workflows/ci.yml`) runs all the gate commands above plus `bun run build` and `bun tauri build` on a 4-platform matrix (macOS arm64/x64, Windows, Linux) — run the gates locally before pushing
- Release: `./publish.sh <patch|minor|major|x.y.z>` (requires a clean tree) bumps versions in `package.json`, `src-tauri/tauri.conf.json`, `Cargo.toml`, `Cargo.lock`, commits, tags `v*`, and pushes; `release.yml` then builds and drafts the GitHub release — the `/r2-release` skill guides the full flow
- Gotcha: `bun install` and builds churn `bun.lock`, `Cargo.lock`, and `package.json` — revert unintended churn before committing

## Architecture

### Frontend (`src/app/`)

- **Framework**: Next.js 16 + React 19, static export (`output: 'export'`) for Tauri
- **UI**: Ant Design 6 + Tailwind CSS 4. Use `App.useApp()` for `message`/`notification`/`modal` — never import static methods from antd
- **State**: Zustand stores in `src/app/stores/` for local UI state; TanStack React Query for server/async state
- **Entry**: `page.tsx` (main UI), `layout.tsx` (providers + global layout)

Key directories:

- `stores/` — Zustand stores: `accountStore`, `syncStore`, `uploadStore`, `downloadStore`, `moveStore`, `batchOperationStore`, `currentPathStore`, `themeStore`, `folderSizeStore`, `previewStore`, `renameStore`, `toastStore`, `reportStore`
- `hooks/` — `useFilesSync.ts` (sync orchestration), `useR2Files.ts` (cached file listing), `useLazySync.ts` + `useBackgroundSync.ts` (sync pipeline)
- `lib/` — `r2cache.ts` (routes sync to provider adapters)
- `lib/diagnostics/` — in-memory session log (console warn/error, uncaught errors, secrets redacted at write time) installed from `providers.tsx`; `lib/report/` builds the app-info block and the prefilled GitHub new-issue URL (kept under `MAX_ISSUE_URL_LENGTH`; log lines drop before the description)
- `components/report/` — "Report a problem" status-bar button + modal (also reachable from the command palette via `reportStore`); it opens the prefilled issue in the browser, nothing is posted from the app
- `providers/` — per-provider frontend sync adapters (`r2/`, `aws/`, `minio/`, `rustfs/`); distinct from `providers.tsx` (React context providers)
- `components/` — Feature-specific modals and views (ConfigModal, BatchMoveModal, FilePreviewModal, etc.)
- `utils/` — Helpers (`formatBytes`, `fileIcon`)

### Backend (`src-tauri/src/`)

- **Framework**: Tauri v2 with Rust
- **Entry**: `main.rs` → `lib.rs` (plugin registration, IPC commands, window management)
- **Database**: SQLite via Turso (`turso` crate). Schema/queries in `db/`
- **Provider commands**: `commands/{r2,aws,minio,rustfs}_commands.rs`, plus `commands/batch_move.rs`, `commands/lazy_sync.rs`, `commands/file_cache.rs` and cache-maintenance modules
- **S3 client**: `providers/s3_client.rs` — shared AWS SDK S3 client factory plus `describe_s3_error` (the only way to turn an `SdkError` into user-facing text — plain `Display` prints just "service error" and drops the code/message); provider adapters in `providers/aws/`, `providers/minio/`, `providers/rustfs.rs`. The R2 adapter is NOT under `providers/` — it lives at top-level `src-tauri/src/r2/`
- **File operations**: `upload.rs`, `download/`, `move_transfer/`, `transfer_progress.rs`
- **Workspace crate**: `crates/range-dl/` — multi-threaded ranged download engine; the reason cargo commands need `--workspace`
- **DB modules**: `db/` — per-provider account/bucket modules (`aws_accounts.rs`, `minio_buckets.rs`, …) following the provider pattern, plus `file_cache.rs`, `dir_tree.rs`, `downloads.rs`, `move_sessions.rs`, `tokens.rs`, `prefix_sync.rs`, `sessions.rs`, `app_state.rs`

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

### Debugging with tauri-connector

[tauri-connector](https://github.com/dickwu/tauri-connector) provides deep inspection and interaction with the running app via an embedded MCP server + CLI. Enabled via `--features connector`.

```bash
bun run tauri:dev            # Starts app with connector enabled (port 9555 WS, 9556 MCP)
tauri-connector snapshot -i  # AI DOM snapshot with refs and React component names
tauri-connector click @e5    # Click element by ref
tauri-connector fill @e3 "text"  # Fill input
tauri-connector screenshot /tmp/shot.png  # Screenshot
tauri-connector logs -n 20   # Console logs
tauri-connector state        # App metadata
```

## Key Conventions

- **Package manager**: Bun (not npm/yarn)
- **Formatting**: Prettier with `prettier-plugin-tailwindcss`
- **Naming**: PascalCase for components, camelCase for variables/functions
- **Antd message API**: Always use `const { message } = App.useApp()` — never `import { message } from 'antd'`
- **Provider pattern**: Each storage provider (R2, AWS, MinIO, RustFS) has parallel command, DB, and provider modules — keep them consistent when adding features
- **Accounts are scoped**: Sync data and file cache are per-account + per-bucket, never mixed across accounts
- **Tests**: colocated `*.test.ts` files run by `bun test` (Bun's built-in runner — don't add vitest/jest); Rust tests are inline `#[cfg(test)]` plus `crates/range-dl/tests/`
- **Static export outputs to `dist/`** (`distDir` in next.config.ts); Tauri consumes `../dist` — there is no `out/`
- **`src-tauri/capabilities/connector.json` is generated** by build.rs under the connector feature and gitignored — never hand-edit
- **AGENTS.md**: gitignored near-copy of this file for Codex — mirror CLAUDE.md edits into it

<!-- BEGIN:nextjs-agent-rules -->

# This is NOT the Next.js you know

This version has breaking changes — APIs, conventions, and file structure may all differ from your training data. Read the relevant guide in `node_modules/next/dist/docs/` (resolved from this file's directory; in monorepos the `next` package may not be visible from the repo root) before writing any code. Heed deprecation notices.

This block is written and re-added by `next dev` — verify at `node_modules/next/dist/server/lib/generate-agent-files.js`. Removing it from a diff only re-creates the uncommitted change; committing it with your work keeps the tree clean.

<!-- END:nextjs-agent-rules -->
