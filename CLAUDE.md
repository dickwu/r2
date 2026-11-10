# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Cloudflare R2 Client — a cross-platform desktop app for managing S3-compatible storage (Cloudflare R2, AWS S3, MinIO, RustFS). Built with Tauri v2 (Rust backend) + Next.js (React frontend).

## Commands

```bash
bun run ffmpeg:fetch     # ffmpeg sidecar for src-tauri/binaries — once per checkout, before any cargo command
bun run dev              # Next.js dev server (port 3000, turbopack)
bun run tauri dev        # Full Tauri desktop app in dev mode
bun run tauri build      # Production build
bun run check            # format:check + typecheck + bun test — the frontend CI gate
```

Rust backend (from `src-tauri/`):

```bash
cargo test --workspace   # Rust tests (root crate + crates/range-dl)
cargo fmt --all --check                                # CI gate
cargo clippy --workspace --all-targets -- -D warnings  # CI gate
```

## CI & Releases

- CI (`.github/workflows/ci.yml`) runs, on a 4-platform matrix (macOS arm64/x64, Windows, Linux): the seven offline acceptance-harness self-tests (`node scripts/r2-audit-manifest.mjs --self-test`, `node scripts/r2-protocol-audit.mjs --self-test`, `node scripts/aws-s3-protocol-audit.mjs --self-test`, `node scripts/provider-fault-audit.mjs --self-test`, `node scripts/vm-boundary-audit.mjs --self-test`, `python3 scripts/vm-powerloss-audit.py --self-test`, `python3 scripts/run-native-nfs-test.py --self-test`), all the gate commands above (Windows only compiles the Rust tests: `cargo test --no-run`), the native NFS steps (Linux: `python3 scripts/run-native-nfs-test.py --sudo`; Windows: `--validate-only`, a prerequisite check that mounts nothing), then `bun run build` and `bun tauri build` — run the gates and the self-tests locally before pushing; `bun run check` never builds, so also run `bun run build` after a dependency change
- `.github/workflows/windows-native.yml` is the separate Windows behavioral gate (`workflow_dispatch`, or PRs touching `src-tauri/**` or its scripts — a direct push to `main` never starts it, so after one that touches those paths run `gh workflow run windows-native.yml --ref main`): a `windows-2022` runner with SDK 10.0.19041.0 runs the full Rust suite plus three real NFS mount/write/read/rename/unmount iterations and uploads `windows-native-nfs-behavior.json`; `docs/engineering/r2-audit/verification.md` says how to land that artifact as acceptance evidence. The native step sets `R2_NFS_TRACE`, which streams nfsserve's RPC trace into the log, plus the `mount.exe`/`net use` state when an operation fails. Gotcha: Client for NFS mounts an exported root as the documented `\\ip\!` — `\\ip\` and `ip:/` map the server itself (`mount.exe` exits 0, the drive lists as `Unavailable`, every path fails with error 53). `mount.exe` reads a `key=value` option to the end of its argument, commas included, so every option needs an `-o` of its own (a comma list applies at most its first option, and still mounts); the bare `casesensitive` Microsoft documents is invalid (`casesensitive=yes` works), `timeout=30` and `timeout=60` list as 10.0 (the effective timeout was never measured), it prints its errors to stdout, not stderr, and the native smoke test asserts the `mount.exe` listing. The client checks mode bits itself, as its anonymous UID -2, so the server reports 0777/0666 to it (0555/0444 read-only); with 0644 it opened no existing file for writing ("Access is denied"). It reuses a server's cached root handle without a MOUNT call, even after `umount`, so Windows mounts never reuse a 127.88.x.y within a run and no handle — the root's included — is accepted from another mount. It sends names in the ANSI code page (`lang=ansi`): a character outside it arrives as `?` (refused) or as a best-fit ASCII look-alike (`čas.txt` is stored as `cas.txt`, undetectably); unless the code page is UTF-8, other non-ASCII characters arrive as code-page bytes, refused when they are not valid UTF-8 and otherwise stored under the key they happen to spell (GBK `说` is `CB B5`, stored as `˵`) — so only ASCII names are reliable on Windows
- Release: `./publish.sh <patch|minor|major|x.y.z>` (requires a clean tree) bumps versions in `package.json`, `src-tauri/tauri.conf.json`, `Cargo.toml`, `Cargo.lock`, commits, tags `v*`, and pushes; `release.yml` then builds and drafts the GitHub release — the `/r2-release` skill guides the full flow. It refuses to tag a version without an entry in r2-home's `data/releases.json`
- Release notes: one entry per version in r2-home's `data/releases.json` (the website repo, `$R2_HOME_DIR` or a sibling checkout) is the single source. r2-home publishes it as `https://r2.lifefarmer.ca/releases.json` (+ `/releases/<v>.json`, CORS `*`, 5-minute cache; drafts included so notes exist before a tag, hidden from the site); `bun scripts/release-notes.ts <v>` renders the GitHub Release body from it and `--apply [--publish]` writes it to the release and its `latest.json` `notes` after CI
- Gotcha: `bun install` and builds churn `bun.lock`, `Cargo.lock`, `package.json` and `next-env.d.ts` (Next rewrites its import paths) — revert unintended churn before committing
- Gotcha: the ffmpeg sidecar (`externalBin`; `src-tauri/binaries/r2-ffmpeg-<target-triple>[.exe]`, gitignored) must exist before any cargo command that compiles — without it `tauri-build` fails build, check, clippy and test. `bun run ffmpeg:fetch` downloads the pinned, sha256-checked build (`bun scripts/fetch-ffmpeg.ts --target <triple>` for another target); only `tauri:dev` and `tauri build` run it for you, so run it once in each fresh clone or worktree. Bumping the pin in `scripts/fetch-ffmpeg.ts` also means updating `scripts/fetch-ffmpeg.test.ts` and `src-tauri/licenses/FFMPEG-NOTICE.md`
- Gotcha: `public/file-viewer/` (~200 MB, gitignored) is staged by `scripts/copy-file-viewer-assets.mjs` from the `file-viewer-copy-assets` package that `@file-viewer/react-full` pins; `dev` and `build` run it first and skip when the manifest version already matches, so the exported `dist/` and every Tauri bundle carry it
- Gotcha: `public/monaco-editor/` (~25 MB, gitignored) is Monaco's AMD build, staged the same way by `scripts/copy-monaco-assets.mjs` from the installed `monaco-editor` (`min/vs` minus `.d.ts`, plus its `LICENSE` and `ThirdPartyNotices.txt`; skipped when its `manifest.json` version and `layout` marker match and `loader.js`, `editor.main.js`/`.css`, both notices and the five worker bundles are still staged, so bumping `layout` forces a re-copy); Monaco deprecated the AMD build in 0.53, so the script fails the build if an upgrade stops shipping `min/vs/loader.js`; it also fails when the package lacks `editor.main.js`/`.css`, either notice or one of the five `assets/<label>.worker-*.js` bundles

## Architecture

### Frontend (`src/app/`)

- **Framework**: Next.js 16 + React 19, static export (`output: 'export'`) for Tauri
- **UI**: Ant Design 6 + Tailwind CSS 4. Use `App.useApp()` for `message`/`notification`/`modal` — never import static methods from antd
- **State**: Zustand stores in `src/app/stores/` for local UI state; TanStack React Query for server/async state
- **Entry**: `page.tsx` (main UI), `layout.tsx` (providers + global layout)

Key directories:

- `lib/diagnostics/` — in-memory session log (console warn/error, uncaught errors, secrets redacted at write time) installed from `providers.tsx`; `lib/report/` builds the app-info block and the prefilled GitHub new-issue URL (kept under `MAX_ISSUE_URL_LENGTH`; log lines drop before the description)
- `components/report/` — "Report a problem" status-bar button + modal (also reachable from the command palette via `reportStore`); it opens the prefilled issue in the browser, nothing is posted from the app
- `lib/updates/` + `stores/updateStore.ts` + `components/updates/` — app updates: `useAppUpdater` (mounted by the status bar's `UpdateChecker`) checks 3 s after launch and every 30 min; the dialog opens by itself once per version (`updatePrefs.ts`, localStorage), Skip this version only silences that; progress, errors (`updateErrors.ts`: what happened + how to fix, Try again / Download from GitHub), and a restart warning for running transfers, mount uploads, and unsaved editors (`activeWork.ts`, `editorDirtyStore`) with Install now, restart later. Notes come from the r2-home feed (`releaseNotes.ts`: every version between yours and the new one), then `latest.json`'s `body`, then a GitHub link, rendered by `notesHtml.ts` (marked; raw HTML escaped, http(s) links only). After an update a What's New notification shows once (`lastRunVersion`); Settings → Release notes lists every version up to yours. Tauri plugins are reached only through `updaterPlugins.ts` (`setUpdaterPlugins` in tests); in dev `window.__r2UpdaterTest` (`devUpdaterSeam.ts`) fakes an update: `simulate({version, failAt, ...})`, `whatsNew(from)`, `unsavedEdits(true)`, `useReleases(url)`, `reset()`
- `providers/` — per-provider frontend sync adapters (`r2/`, `aws/`, `minio/`, `rustfs/`); distinct from `providers.tsx` (React context providers)
- `components/preview/` — `previewKind.ts` routes a file by extension: browser-native images/video/audio stream from the object URL, text goes to the Monaco `TextViewer` (editable, saves back to the bucket), everything else goes to `FileViewerPane`, which wraps [`@file-viewer/react-full`](https://github.com/flyfish-dev/file-viewer) (PDF, Office, archives, e-books, e-mail, CAD, 3D, design files… ~250 extensions; read-only). Every previewed object is also identified by its content: the `detect_url_type` command fetches the first 3 KB with one ranged GET (a HEAD fails a GET-presigned URL), drops a UTF-8 character the range cut in two (the detector reads such text as binary), and sniffs them with the `mimetype-detector` crate, returning the stored Content-Type alongside, which only refines generic sniffs (plain text, ZIP, OLE, unknown binary). `usePreviewTarget.ts` lets the content decide when the name has no extension a preview handles (e.g. UUID keys), and when the content contradicts a rendering name (`contradictsName`: text sniffs and bare ZIP/OLE containers never contradict a name the viewer opens) it keeps the name's rendering and offers the content's in a chooser; a video/audio type no listed format stands for reaches the native player only if `canPlayType` allows it (WebKit has no Matroska/AVI); an object opened by its content is saved back with its stored type. Gotcha: WebKit picks an `<img>`'s decoder by the served Content-Type (a JPEG stored as `application/pdf` shows blank; SVG renders only as `image/svg+xml`), so an image identified by its content but stored under a non-image type is shown from its downloaded bytes (`BlobImage.tsx`, `getImageBlobType`). Grid thumbnails and the inspector identify extension-less keys only (`hooks/useListedObjectType.ts`, one cached lookup per object). Previews download through `fetchPreviewBytes.ts` (Tauri HTTP plugin, so signed R2/S3 URLs are not blocked by webview CORS) and hand the viewer a named `File` plus an explicit `type`. Gotcha: the plugin's scope patterns (`https://*`, `http://**`) pin the default port, so endpoints with an explicit port (self-hosted MinIO/RustFS on :9000) are always fetched through the backend fallback `fetch_url_bytes`; `fileViewerAssets.ts` pins both the preset's and core's asset base to `<origin>/file-viewer/`, and `monacoAssets.ts` points `@monaco-editor/react`'s loader at `<origin>/monaco-editor/vs` (unconfigured, it fetches a different Monaco from jsdelivr and the editor never loads offline). Gotcha: the viewer's PDF, draw.io and legacy .ppt renderers gate their Workers on an http(s) origin, so on `tauri://localhost` (macOS/Linux builds) they fall back to main-thread parsing or a simplified render rather than failing; the viewer's download/print/export actions are hidden because the webview has no handler for `<a download>` or `window.open()`

### Backend (`src-tauri/src/`)

- **Entry**: `main.rs` → `lib.rs` (plugin registration, IPC commands, window management)
- **Database**: SQLite via Turso (`turso` crate). Schema/queries in `db/`
- **Provider commands**: `commands/{r2,aws,minio,rustfs}_commands.rs`, plus `commands/batch_move.rs`, `commands/lazy_sync.rs`, `commands/file_cache.rs` and cache-maintenance modules
- **S3 client**: `providers/s3_client.rs` — shared AWS SDK S3 client factory plus `describe_s3_error` (the only way to turn an `SdkError` into user-facing text — plain `Display` prints just "service error" and drops the code/message) and `s3_error_class` (`Transient` = 5xx/429, throttling and timeout codes, dropped connections — what a retry may wait out; a mutation gets `OutcomeUnknown` instead). SDK retries are off (`max_attempts(1)`): replayable reads and immutable multipart parts retry only through `providers/operation.rs::execute` (per-endpoint admission, full-jitter backoff, shared deadline, cancellation; `commands/lazy_sync.rs` passes its own list policy and never retries 403/404), while Copy/PUT/Complete/Delete are never blindly re-sent — an uncertain outcome is reconciled first; provider adapters in `providers/aws/`, `providers/minio/`, `providers/rustfs.rs`. The R2 adapter is NOT under `providers/` — it lives at top-level `src-tauri/src/r2/`
- **File operations**: `upload.rs`, `download/`, `move_transfer/`, `transfer_progress.rs`
- **Mount**: `mount/` serves a bucket as a local drive — an in-process NFSv3 server (`nfsserve`) on loopback plus the OS's own NFS client (argv per platform in `platform.rs`). A WRITE is acknowledged only once its bytes are fsynced into the local stage (nfsserve reports every WRITE as durable and has no close hook; see `stage.rs`); the upload follows later, always as a conditional write. Its listings and read cache are in-memory, separate from the SQLite file cache
- **Workspace crate**: `crates/range-dl/` — multi-threaded ranged download engine; the reason cargo commands need `--workspace`
- **DB modules**: `db/` — per-provider account/bucket modules (`aws_accounts.rs`, `minio_buckets.rs`, …) following the provider pattern, plus `file_cache.rs`, `dir_tree.rs`, `downloads.rs`, `move_sessions.rs`, `tokens.rs`, `prefix_sync.rs`, `sessions.rs`, `app_state.rs`

### Sync Pipeline

Bucket contents are cached in SQLite per provider + account + bucket. Two paths fill the cache, both going from `lib/r2cache.ts` into `commands/lazy_sync.rs`:

1. **Opening a folder** (`useR2Files` → `loadFolderItems`): `get_prefix_cache_page` serves the cached rows when the folder's own snapshot is complete or a finished whole-bucket sync covers it; unless the folder was listed under 60 s ago, `list_prefix_stream` re-lists the prefix and streams `folder-page` events
2. **Whole bucket** (`useFilesSync`, on bucket open and on refresh): `start_background_sync` lists into `cached_files_staging`, then one transaction swaps `cached_files` and rebuilds `directory_tree`; `background-sync-*` events feed `syncStore.backgroundSync`

Legacy, with no caller left in the UI: the `sync_bucket` (R2) and `sync_{aws,minio,rustfs}_bucket` commands, `syncBucket`, `hooks/useLazySync.ts`, `list_prefix`, and the `sync-phase`/`sync-progress`/`indexing-progress` events — build on the two paths above.

### Frontend ↔ Backend Communication

- Tauri IPC via `#[tauri::command]` (Rust) and `@tauri-apps/api` (TypeScript)
- HTTP requests use `@tauri-apps/plugin-http`
- Tauri features are conditionally loaded (check `window.__TAURI__`)

### Debugging with tauri-connector

[tauri-connector](https://github.com/dickwu/tauri-connector) provides deep inspection and interaction with the running app via an embedded MCP server + CLI. Enabled via `--features connector`.

```bash
bun run tauri:dev            # Starts app with connector enabled (pinned: 9568 WS, 9567 MCP)
tauri-connector snapshot -i  # AI DOM snapshot with refs and React component names
tauri-connector click @e5    # Click element by ref
tauri-connector fill @e3 "text"  # Fill input
tauri-connector screenshot /tmp/shot.png  # Screenshot
tauri-connector logs -n 20   # Console logs
tauri-connector state        # App metadata
```

The `tauri:dev` script pins the connector to WS 9568 and MCP 9567 (`TAURI_CONNECTOR_PORT` / `TAURI_CONNECTOR_MCP_PORT`) so `.mcp.json` always reaches this app, not another connector app on the default 9555+ range. `.mcp.json` is gitignored, so every checkout carries its own copy: it must point at `http://127.0.0.1:9567/mcp`. Exact ports do not fall back: if one is busy the connector logs an error and does not start. Pass `--project r2` to the CLI when several connector apps run.

## Key Conventions

- **Package manager**: Bun (not npm/yarn)
- **Formatting**: Prettier (`prettier-plugin-tailwindcss`) checks every file type it knows, Markdown, JSON and YAML included — run `bun run format` after editing docs, locale files or workflows too
- **Provider pattern**: Each storage provider (R2, AWS, MinIO, RustFS) has parallel command, DB, and provider modules — keep them consistent when adding features
- **Accounts are scoped**: Sync data and file cache are per-account + per-bucket, never mixed across accounts
- **i18n**: user-visible text goes through i18next (`src/i18n/`) — `const { t } = useTranslation()` in components, `import { t as i18nT } from '@/i18n'` elsewhere, called at use time (a module-scope call freezes the startup language; `KIND_LABEL` in `lib/transferFailure.ts` uses getters instead). Add each key to all five `src/i18n/locales/*.json` (`en`, `zh-CN`, `hi`, `es`, `ja`): nothing checks parity, and a missing key shows the English text, or the raw key when `en` lacks it. Plural keys follow each language (`_one`/`_other`; `es` adds `_many`; `zh-CN` and `ja` have only `_other`). Rust backend text, logs and the GitHub issue report stay English
- **README**: four translations share `README.md`'s structure (`README.zh-CN.md`, `README.hi.md`, `README.es.md`, `README.ja.md`) — edit all five together
- **Tests**: colocated `*.test.ts` files run by `bun test` (Bun's built-in runner — don't add vitest/jest). `bun:test` is typed by the hand-written stub `src/app/utils/bun-test.d.ts` (no `@types/bun`): a matcher it does not declare passes `bun test` but fails `bun run typecheck` — declare it there. Rust tests are inline `#[cfg(test)]` plus `crates/range-dl/tests/`
- **Static export outputs to `dist/`** (`distDir` in next.config.ts); Tauri consumes `../dist` — there is no `out/`
- **Fonts are self-hosted**: `public/fonts/` (woff2 + OFL licenses) + `src/app/fonts.css` — never use `next/font/google`, which fetches Google Fonts at build time and flakily fails `next build` (vercel/next.js#99114)
- **`src-tauri/capabilities/connector.json` is generated** by build.rs under the connector feature and gitignored — never hand-edit
- **macOS Local Network privacy**: `src-tauri/Info.plist` (Tauri merges it into the bundle and embeds it in dev builds) declares `NSLocalNetworkUsageDescription`; without it macOS 15+ silently refuses LAN endpoints (self-hosted RustFS/MinIO) with "No route to host" (os error 65), and `describe_s3_error` leads that failure with the System Settings path
- **AGENTS.md**: gitignored and local to each checkout — a short pointer to this file for Codex (`next dev` may add its `nextjs-agent-rules` block there). Project instructions live in this file only

<!-- BEGIN:nextjs-agent-rules -->

# This is NOT the Next.js you know

This version has breaking changes — APIs, conventions, and file structure may all differ from your training data. Read the relevant guide in `node_modules/next/dist/docs/` (resolved from this file's directory; in monorepos the `next` package may not be visible from the repo root) before writing any code. Heed deprecation notices.

This block is written and re-added by `next dev` — verify at `node_modules/next/dist/server/lib/generate-agent-files.js`. Removing it from a diff only re-creates the uncommitted change; committing it with your work keeps the tree clean.

<!-- END:nextjs-agent-rules -->
