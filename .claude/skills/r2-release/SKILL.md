---
name: r2-release
description: >
  Publish a new release of the R2 Client desktop app and monitor CI.
  Use when the user asks to "publish a release", "cut a release", "bump the version",
  "release a new version", "check ci" after changes are committed, or
  "write release notes" / "publish release notes".
  Handles pre-flight checks, version bumping, git tagging, pushing, CI monitoring,
  and generating release notes from git diff using the project's publish.sh script
  and the gh CLI.
---

# R2 Client Release Workflow

The project has a `./publish.sh` script that handles the full release pipeline:
1. Validates version argument (patch/minor/major or explicit x.y.z)
2. Checks for clean working tree (uncommitted changes = error)
3. Updates `package.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock`
4. Commits, creates annotated tag, pushes to origin — triggers GitHub Actions

## Pre-flight: Ensure Clean Working Tree

`publish.sh` requires a clean working directory. Any uncommitted changes must be
committed **before** running the script.

```bash
git status --short
```

If there are uncommitted changes, commit them first with an appropriate conventional
commit message, then proceed.

## Running the Release

`publish.sh` accepts: `patch`, `minor`, `major`, or an explicit `x.y.z` version.
Default to `patch` unless the user specifies otherwise.

```bash
# Show current version and bump examples
./publish.sh

# Run the release (replace <arg> with patch/minor/major/x.y.z)
./publish.sh <arg>
```

The script handles all version arithmetic, file updates, git commit, tag creation,
and push — do NOT duplicate any of that logic manually.

## Checking CI After Push

After the script exits successfully, immediately check CI:

```bash
gh run list --limit 5
```

Two workflows trigger per release:
- **CI** (branch push on `main`) — cross-platform build validation
- **Release** (tag push `v*`) — builds + signs artifacts + publishes GitHub draft release

Platforms built: macOS arm64, macOS x64, Windows, Ubuntu (Linux).

### Monitoring

```bash
# Watch a specific run to completion (blocks until done)
gh run watch <run-id> --exit-status

# Quick status poll (useful for long builds — poll every ~2 min)
gh run list --limit 4
```

Tauri cross-platform builds typically take **7-15 minutes**.

### Reporting Status

When both runs reach `completed success`, report:
- CI run: status + duration
- Release run: status + duration
- Release URL: `https://github.com/dickwu/r2/releases/tag/v<version>`

## Release Notes

After CI succeeds, generate release notes and publish them to the GitHub release.
The Release workflow creates a **draft** release with artifacts — this step adds
notes and marks it as published.

### Generating Notes from Git Diff

```bash
# Find the previous release tag
git tag --sort=-v:refname | head -2

# Commits since last release (use for the changelog summary)
git log --oneline <prev-tag>..<new-tag> --no-merges

# File-level change stats (exclude version-bump-only files)
git diff --stat <prev-tag>..<new-tag> -- ':!src-tauri/Cargo.lock' ':!src-tauri/Cargo.toml' ':!src-tauri/tauri.conf.json' ':!package.json'
```

### Writing Notes

Categorize commits using conventional-commit types:
- **feat:** -> "New Features"
- **fix:** -> "Bug Fixes"
- **refactor/perf:** -> "Improvements"
- **chore/ci/docs:** -> "Maintenance"

Template:

```markdown
## What's Changed

### New Features
- <description from feat commits>

### Bug Fixes
- <description from fix commits>

### Improvements / Maintenance
- <description from chore/refactor commits>

**Full Changelog**: https://github.com/dickwu/r2/compare/v<prev>...v<new>
```

Omit empty sections. Keep bullet points concise (one line each).

### Publishing

```bash
gh release edit v<version> --draft=false --notes "$(cat <<'EOF'
## What's Changed
...
**Full Changelog**: https://github.com/dickwu/r2/compare/v<prev>...v<new>
EOF
)"
```
