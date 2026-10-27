#!/usr/bin/env bun
/**
 * Release notes for one version, rendered from r2-home's data/releases.json, the
 * same entry the website changelog and the in-app update dialog read.
 *
 *   bun scripts/release-notes.ts 0.3.18                  print the GitHub Release body
 *   bun scripts/release-notes.ts 0.3.18 --apply          write it to the v0.3.18 release
 *                                                        and to that release's latest.json
 *   bun scripts/release-notes.ts 0.3.18 --apply --publish   ...and publish the draft release
 *   --source <path|url>   read another file or a feed URL
 *
 * The source defaults to $R2_HOME_DIR/data/releases.json, then a sibling r2-home
 * checkout, then the published feed.
 */
import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  RELEASES_URL,
  findRelease,
  normalizeVersion,
  parseReleaseFeed,
  releaseBody,
} from '../src/app/lib/updates/releaseNotes';

const fail = (message: string): never => {
  console.error(`release-notes: ${message}`);
  process.exit(1);
};

const args = process.argv.slice(2);
const flag = (name: string) => args.includes(name);
const option = (name: string) => {
  const index = args.indexOf(name);
  return index >= 0 ? args[index + 1] : undefined;
};
const version = args.find((arg, i) => !arg.startsWith('--') && args[i - 1] !== '--source');
if (!version)
  fail('usage: bun scripts/release-notes.ts <version> [--source path|url] [--apply [--publish]]');

const scriptDir = dirname(fileURLToPath(import.meta.url));

const defaultSource = (): string => {
  const candidates = [
    process.env.R2_HOME_DIR && join(process.env.R2_HOME_DIR, 'data/releases.json'),
    resolve(scriptDir, '../../r2-home/data/releases.json'),
    resolve(scriptDir, '../../../opensource/r2-home/data/releases.json'),
  ].filter((p): p is string => Boolean(p));
  return candidates.find((p) => existsSync(p)) ?? RELEASES_URL;
};

const source = option('--source') ?? defaultSource();
const raw: unknown = /^https?:\/\//.test(source)
  ? await fetch(source).then((r) => (r.ok ? r.json() : fail(`${source} returned HTTP ${r.status}`)))
  : JSON.parse(readFileSync(source, 'utf8'));

const feed = parseReleaseFeed(raw) ?? fail(`${source} isn't a release list`);
const v = normalizeVersion(version!);
const entry =
  findRelease(feed, v) ??
  fail(`${source} has no entry for ${v}. Add it to r2-home's data/releases.json before releasing.`);
const body = releaseBody(feed, v)!;

if (entry.draft) console.error(`release-notes: ${v} is still marked draft in ${source}`);
if (!entry.notes.new.length && !entry.notes.improved.length && !entry.notes.fixed.length) {
  console.error(`release-notes: ${v} has a summary but no itemized notes`);
}

if (!flag('--apply')) {
  console.log(body);
  process.exit(0);
}

const run = (cmd: string[]) => {
  const result = spawnSync(cmd[0], cmd.slice(1), { stdio: 'inherit' });
  if (result.status !== 0) fail(`${cmd.slice(0, 3).join(' ')} failed`);
};

const tag = `v${v}`;
const dir = mkdtempSync(join(tmpdir(), 'r2-release-notes-'));
try {
  const notesFile = join(dir, 'notes.md');
  writeFileSync(notesFile, `${body}\n`);
  run(['gh', 'release', 'edit', tag, '--notes-file', notesFile]);
  console.error(`release-notes: updated the ${tag} release body`);

  // latest.json is what the in-app updater reads; its notes are the fallback
  // when the website feed can't be reached.
  run(['gh', 'release', 'download', tag, '--pattern', 'latest.json', '--dir', dir, '--clobber']);
  const latestPath = join(dir, 'latest.json');
  const latest = JSON.parse(readFileSync(latestPath, 'utf8')) as Record<string, unknown>;
  latest.notes = body;
  writeFileSync(latestPath, `${JSON.stringify(latest, null, 2)}\n`);
  run(['gh', 'release', 'upload', tag, latestPath, '--clobber']);
  console.error(`release-notes: updated latest.json on ${tag}`);

  if (flag('--publish')) {
    run(['gh', 'release', 'edit', tag, '--draft=false']);
    console.error(`release-notes: published ${tag}`);
  }
} finally {
  rmSync(dir, { recursive: true, force: true });
}
