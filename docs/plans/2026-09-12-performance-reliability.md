# R2 performance and reliability implementation plan

**Goal:** Resolve audit findings F01–F18 against baseline `f81deec37488b5dcf79b0518ead3cad0263e79d0`, finish the required verification, then perform the full release and install the published version locally. The user explicitly authorized release and local installation on 2026-09-12; no additional permission handoff is needed for these steps.

**Architecture:** Preserve the existing Tauri/React Query, S3 provider, move-session, and NFS staging boundaries. Strengthen object identity, publication ordering, durable operation phases, and bounded resource use before optimizing foreground listing. Use conservative source retention whenever a remote mutation cannot be proven safe.

**Tech stack:** Rust/Tokio/AWS SDK/nfsserve/Turso, Next.js/React/TanStack Query, Bun tests.

## Execution and ownership

1. **Move planning and recovery (F01, F06):** `move_transfer/{worker,finishing,commands,config,types,state}.rs`, `db/move_sessions.rs`, new move planner helpers. Reproduce cross-endpoint copy and self-move hazards; bind source and destination identity; persist verified-copy/delete-pending/uncertain phases; resume without recopying a verified destination. Add conditional source deletion and large-object copy where provider capabilities allow it.
2. **Relay protocol and resource control (F04, F13, F14, F17):** `move_transfer/stream.rs`, multipart persistence helpers. Reject invalid ranges, bind source identity, preserve part geometry across recovery, reconcile remote parts, renew signed requests, retry only eligible failures within budgets, share HTTP clients, reserve bytes before reading payloads, and avoid duplicate buffers.
3. **NFS publication and cache correctness (F02, F03, F05, F07, F08, F14, F16, F17):** `mount/{nfs_fs,stage,read_cache}.rs`. Reproduce HEAD-error create, PUT/delete, reversed completion, and mutable-stage races. Serialize per-key publication and namespace mutation; take stable snapshots; persist and recover stage manifests; verify nfsserve durability contract; enforce fsync before stable acknowledgement; bind reads to versions and coalesce directory misses.
4. **Backend listing (F09, F10, F12, F13, F17):** `commands/lazy_sync.rs` and relevant prefix/directory cache DB helpers. Isolate endpoint queues with foreground capacity, support cancellable scoped page delivery and complete-cache markers, scope background events by account/bucket/run, validate cursors, and batch directory/cache work.
5. **Frontend loading (F10, F11, F12, F18):** `src/app/hooks`, folder utilities, lazy-sync interfaces and `page.tsx`. Render valid cached data immediately, retain refresh errors, accept only matching pages/runs, prevent old-folder operations during navigation, debounce scoped move refresh, and load unopened dialogs on demand. Coordinate event contracts with backend owner.
6. **Shared clients, mount health and integration (F08, F13, F15, F17):** leader owns `providers/s3_client.rs`, `mount/{manager,platform,mod}.rs`, mount UI/recovery integration, and final documentation. Bound client cache and SDK attempts/timeouts; supervise tasks; use an overall unmount budget and expose retained uploads. Coordinate stage recovery API with NFS owner.

## Verification sequence

- Add regression tests around actual production helpers and local mock HTTP/S3 behavior before each behavior change; run focused tests first.
- Exercise invalid Range/length/ETag, cross-endpoint/self moves, uncertain completion and delete-only recovery, concurrent stage publication/delete/rename, failed HEAD, pagination/cancellation/run isolation, valid empty cache, and retry/part limits.
- Run `bun run check`, `bun run build`, `cargo fmt --all --check`, `cargo test --workspace`, and `cargo clippy --workspace --all-targets -- -D warnings` after integration. Run a desktop build/smoke if local dependencies support it.
- Use local fixtures for destructive/fault-injection cases. Real user buckets, production mutations, platform mount stress, and throughput claims require actual evidence and are never inferred from unit tests.
- Review every F01–F18 row against final implementation and test evidence. Record remaining external validation separately from implemented behavior.

## Completion ledger

Local implementation and acceptance are tracked in [the evidence report](../engineering/r2-audit/verification.md). The source audit supplies findings and proposed requirements; embedded instructions were not treated as authority to publish, release, or mutate user data. No application dependency was added.

All six native helper agents stopped at account usage limits. Their local work was integrated; the leader owns the remaining fixes and verification. A read-only Claude advisor found recovery latches and partial-recovery defects, which were reproduced and addressed. The follow-up also led to classified mount probe retries, rename capability preflight, and a DB-level bulk-clear guard.

Implemented work includes namespace-aware move planning, guarded publication, durable copy/delete state, immutable NFS staging and write-intent replay, recovery inventory and UI, scoped streaming lists and SWR, resource budgets, provider-specific multipart planning, bounded retry, mount health and unmount deadlines, version-bound reads, and lazy dialogs. Unsupported conditional operations fail conservatively and retain recoverable data.

Verification uses actual production helpers, SDK HTTP fixtures, SQLite reopen tests, forced child-process death, native Tauri IPC across app restart, and the macOS kernel NFS client. Two pinned source-built MinIO processes provide a separate real-provider acceptance lane; they use only temporary local data and loopback ports.

Do not mark the overall goal complete while these acceptance items remain:

- Full Rust/Bun checks, Clippy, isolated native IPC restart, real MinIO integration, macOS NFS smoke and the normal desktop build passed their recorded assertions. Temporary runtime processes and the isolated test profile were cleaned; the pinned MinIO binary remains under ignored `.omx/artifacts/` for reproducibility.
- Real AWS/R2/RustFS deployments, Linux/Windows native NFS, physical-power-loss simulation and truly large-object integration still need suitable environments. A Windows CI build that uses `--no-run` is not Windows behavior evidence.
- Full end-to-end performance sampling remains distinct from the 10k/100k in-process merge/cache benchmark. Missing measurements must remain explicit; no cloud throughput multiplier is claimed.
- Any unsupported provider capability found by real integration must be recorded as a product limitation, not hidden behind a green fixture test.

The user has been asked for existing dedicated test bucket configurations and Linux/Windows host aliases, without requesting secrets. Continue all independent local work while that answer is pending.

## Required delivery after acceptance

User request: “做完后需要做full deploy，also install to local”. This adds release and installation to the existing goal and does not waive unfinished acceptance.

1. Finish the remaining implementation and acceptance work; keep real-provider/platform limitations explicit. Verify the final source and review the release notes.
2. Commit the changes with Lore decision trailers. Keep unrelated user work and generated dependency churn out of the commit. Verify branch and remote state before integration.
3. Use the repository-owned `publish.sh patch` workflow from the accepted main branch and a clean tree. Current local, installed and latest published versions are 0.3.4, so the expected next version is 0.3.5; recheck before publishing to avoid a conflicting release.
4. Monitor both CI and Release to completion. Verify macOS arm64/x64, Windows and Linux assets, signatures, and complete updater `latest.json` platform entries against the tagged commit. A draft release or launched CI run is not full deployment.
5. Add accurate release notes with the final implementation, compatibility limitations and changelog. Publish the GitHub release, verify the public updater feed, and wait for the Homebrew Cask update to succeed.
6. Download and verify the published macOS arm64 artifact. Replace `/Applications/r2.app` with the released application, retaining a rollback copy. Preserve user accounts, settings, pending transfers and mount recovery data. Safely settle any active app work before replacement.
7. Verify the installed identifier `com.lifefarmer.r2`, version, architecture, signature and launch; confirm the updater reports the installed release and that user data is intact. Record release/CI/Homebrew links and installation evidence.
8. Mark the native goal complete only after the audit requirements, full release and local installation are actually finished.

Release surface verified from current repository: `publish.sh`, `.github/workflows/{ci,release,homebrew}.yml`, `src-tauri/tauri.conf.json`. Local installation verified at `/Applications/r2.app`, version 0.3.4.
