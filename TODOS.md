# TODOS

## Design System Documentation

**What:** Create DESIGN.md documenting color tokens, spacing scale, typography, and component patterns.
**Why:** Prevents dark mode bugs and inconsistent styling as the app grows. The design review (5/10 → 9/10) found hardcoded hex colors in DownloadTaskItem.tsx and StatusIcon components. CSS custom properties exist (`var(--color-*)`) but aren't documented.
**Pros:** Single source of truth for design decisions. New contributors can self-serve.
**Cons:** Requires ~30 min to create comprehensively.
**Depends on:** Nothing. Can be done anytime via `/design-consultation`.
**Added:** 2026-03-22 by /plan-eng-review

## Test Infrastructure: Script + CI

**What:** Add `"test": "bun test"` to package.json scripts and a test step to the GitHub Actions CI workflow (.github/workflows/ci.yml).
**Why:** The project now has its first test file (renameKey.test.ts from issue #2), but no way to run tests in CI. `bun test` works locally with zero config, but CI only builds and doesn't run tests. Without a CI step, tests rot silently.
**Pros:** Tests run on every PR. Catches regressions before merge. Establishes the testing pattern for future features.
**Cons:** Minor CI time increase (~5 seconds for `bun test`). Requires verifying CI has bun installed.
**Context:** The R2 Client had zero test files before issue #2. The renameKey utility is the first tested code. As more features add tests, CI enforcement becomes important. Start with `bun test` (built-in runner, no new deps). The CI workflow at `.github/workflows/ci.yml` needs a new step after `bun install`.
**Depends on:** Issue #2 (auto-rename feature) being merged first, since it introduces the first test file.
**Added:** 2026-03-31 by /plan-eng-review
