# TODOS

## Design System Documentation

**What:** Create DESIGN.md documenting color tokens, spacing scale, typography, and component patterns.
**Why:** Prevents dark mode bugs and inconsistent styling as the app grows. The design review (5/10 → 9/10) found hardcoded hex colors in DownloadTaskItem.tsx and StatusIcon components. CSS custom properties exist (`var(--color-*)`) but aren't documented.
**Pros:** Single source of truth for design decisions. New contributors can self-serve.
**Cons:** Requires ~30 min to create comprehensively.
**Depends on:** Nothing. Can be done anytime via `/design-consultation`.
**Added:** 2026-03-22 by /plan-eng-review
