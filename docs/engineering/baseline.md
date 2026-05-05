# Baseline

Date: 2026-05-04 15:37:08 CDT
Commit: 6126cbf
OS: macOS 26.4.1 25E253 arm64
Bun: 1.3.13
Rust: rustc 1.95.0 (59807616e 2026-04-14) (Homebrew)
Node: v25.9.0

## Commands

| Command                | Result | Notes                                                                                                        |
| ---------------------- | -----: | ------------------------------------------------------------------------------------------------------------ |
| bun install            |   pass | Installed 329 packages with Bun 1.3.13.                                                                      |
| bun run format:check   |   fail | Pre-existing Prettier drift in `next-env.d.ts`, `src/app/components/SyncBanner.tsx`, and `src/app/page.tsx`. |
| bun run build          |   pass | Next.js production build completed.                                                                          |
| cargo check            |   pass | Ran from `src-tauri/`.                                                                                       |
| cargo test --workspace |   pass | Ran from `src-tauri/`; 16 tests passed, 1 doctest ignored.                                                   |
