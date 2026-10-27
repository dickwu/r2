---
name: r2-release
description: >
  Publish a new release of the R2 Client desktop app and monitor CI.
  Use when the user asks to "publish a release", "cut a release", "bump the version",
  "release a new version", "check ci" after changes are committed, or
  "write release notes" / "publish release notes".
  Handles pre-flight checks, the shared release-notes entry in r2-home, version
  bumping, git tagging, pushing, CI monitoring, and applying the notes to the GitHub
  Release and latest.json with publish.sh, scripts/release-notes.ts, and the gh CLI.
---

# R2 Client Release Workflow

The project has a `./publish.sh` script that handles the full release pipeline:

1. Validates version argument (patch/minor/major or explicit x.y.z)
2. Checks that r2-home has a release-notes entry for the new version (missing = error)
3. Checks for clean working tree (uncommitted changes = error)
4. Updates `package.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock`
5. Commits, creates annotated tag, pushes to origin — triggers GitHub Actions

## Release Notes Live in r2-home

One entry per version in r2-home's `data/releases.json`
(`/Users/lifefarmer/opensource/r2-home`, or `$R2_HOME_DIR`) feeds four places:

- the website changelog (generated into `index.html` by `npm run build` in r2-home)
- the public feed `https://r2.lifefarmer.ca/releases.json` and `/releases/<v>.json`,
  which the app's update dialog, What's New notice, and Settings → Release notes read
- the GitHub Release body
- the `notes` field of the release's `latest.json` (the updater's fallback when the feed
  can't be reached)

Write the entry **before** running `publish.sh` (it refuses to tag without one). Mark it
`"draft": true` with `"date": null` until the release is out: the website hides drafts,
but the feed carries them, so the update dialog has notes the moment CI publishes.

```json
{
  "version": "0.3.18",
  "date": null,
  "draft": true,
  "title": "Short headline in sentence case",
  "summary": "One or two plain sentences for the changelog card.",
  "notes": {
    "new": ["What someone can now do."],
    "improved": ["What works better."],
    "fixed": ["What was broken and now works."]
  }
}
```

Build the notes from the commits since the last tag (`git log --oneline <prev-tag>..HEAD --no-merges`),
written for users: what changed for them, one line each, no commit prefixes. Omit empty groups.
Then in r2-home run `npm run build` and commit. Preview the GitHub body from r2 with
`bun scripts/release-notes.ts <version>`.

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

After CI succeeds the Release workflow has created a **draft** release with artifacts.
Apply the shared notes and publish it:

```bash
# Writes the release body and latest.json notes, then publishes the draft release
bun scripts/release-notes.ts <version> --apply --publish
```

Then finish the r2-home entry: set `"date"` to the release day (YYYY-MM-DD), remove
`"draft"`, run `npm run build`, commit, and push r2-home `main` (pushing deploys the site,
so the changelog, `latest` in the feed, and the version on the page all move together).

To fix notes after the fact, edit the entry in r2-home, push the site, and rerun
`bun scripts/release-notes.ts <version> --apply`.
