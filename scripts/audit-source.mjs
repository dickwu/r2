#!/usr/bin/env node
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, realpathSync, statSync, readdirSync } from 'node:fs';
import { resolve, relative, sep } from 'node:path';

const DEFAULT_ROOT = resolve(import.meta.dirname, '..');

const EXACT_INPUTS = new Set([
  'package.json',
  'bun.lock',
  'next.config.js',
  'next.config.mjs',
  'next.config.ts',
  'tsconfig.json',
  'postcss.config.js',
  'postcss.config.mjs',
  'tailwind.config.js',
  'tailwind.config.ts',
  'components.json',
  '.cargo/config',
  '.cargo/config.toml',
  'src-tauri/.cargo/config',
  'src-tauri/.cargo/config.toml',
  'src-tauri/Cargo.toml',
  'src-tauri/Cargo.lock',
  'src-tauri/build.rs',
  'src-tauri/tauri.conf.json',
  'src-tauri/tauri.conf.json5',
]);

const INCLUDED_DIRS = [
  'src',
  'public',
  'src-tauri/src',
  'src-tauri/crates',
  'src-tauri/icons',
  'src-tauri/capabilities',
  'src-tauri/capabilities-dev',
  'src-tauri/resources',
];
const EXCLUDED_PREFIXES = [
  'docs/',
  'scripts/',
  '.omx/',
  '.github/',
  'target/',
  'src-tauri/target/',
  'node_modules/',
  '.next/',
  'out/',
  'dist/',
];
const EXCLUDED_SUFFIXES = ['.map', '.log'];

function toRepoPath(root, path) {
  return relative(root, path).split(sep).join('/');
}

// process.argv[1] is left exactly as invoked (a symlink path is never
// resolved), while import.meta.url/import.meta.filename is Node's resolved
// module identity. A plain string/path comparison between the two silently
// no-ops when this script is run through a symlink; comparing real paths
// makes the entrypoint check symlink-safe. Never throw on a non-existent or
// unresolvable argv[1] (e.g. a bare `bun test` runner path) — that just
// means this module was imported, not executed directly.
export function isMainModule(argv1, moduleFilename) {
  if (!argv1) return false;
  try {
    return realpathSync(argv1) === realpathSync(moduleFilename);
  } catch {
    return false;
  }
}

// Resolves a JSON pointer against a document; undefined when any step of the
// path is absent.
export function jsonPointer(document, expression) {
  if (!expression) return undefined;
  if (expression === '') return document;
  if (!expression.startsWith('/')) throw new Error(`JSON pointer must start with /: ${expression}`);
  return expression
    .slice(1)
    .split('/')
    .reduce((current, part) => {
      if (current === undefined || current === null) return undefined;
      const key = part.replaceAll('~1', '/').replaceAll('~0', '~');
      return current[key];
    }, document);
}

// Evidence and generated status are written into this directory before they
// are committed, so uncommitted files there never make a worktree dirty for
// provenance purposes. The manifest and the hand-written verification.md are
// acceptance inputs, not outputs, and are never exempt.
const AUDIT_OUTPUT_DIR = 'docs/engineering/r2-audit/';
const AUDIT_INPUTS = new Set([
  'docs/engineering/r2-audit/acceptance-manifest.json',
  'docs/engineering/r2-audit/verification.md',
]);

function isAuditOutput(path) {
  return path.startsWith(AUDIT_OUTPUT_DIR) && !AUDIT_INPUTS.has(path);
}

// Paths `git status --porcelain -z` reports as modified, staged, renamed or
// untracked. A rename lists the new path and then, as its own entry, the
// original path.
function porcelainPaths(output) {
  const entries = output.split('\0');
  const paths = [];
  for (let index = 0; index < entries.length; index++) {
    const entry = entries[index];
    if (!entry) continue;
    paths.push(entry.slice(3));
    if (/[RC]/.test(entry.slice(0, 2))) paths.push(entries[++index]);
  }
  return paths;
}

// The commit and tree a run is bound to, plus whether anything other than
// audit outputs was uncommitted at the time. Every field is null when git or
// a repository is unavailable; nothing is ever fabricated.
export function gitProvenance(root = DEFAULT_ROOT) {
  const runGit = (args) =>
    execFileSync('git', args, {
      cwd: resolve(root),
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      timeout: 60_000,
    });
  try {
    const commit = runGit(['rev-parse', 'HEAD']).trim();
    const tree = runGit(['rev-parse', 'HEAD^{tree}']).trim();
    const dirtyPaths = porcelainPaths(
      runGit(['status', '--porcelain', '-z', '--untracked-files=all'])
    )
      .filter((path) => !isAuditOutput(path))
      .sort();
    return { commit, tree, dirty: dirtyPaths.length > 0, dirty_paths: dirtyPaths };
  } catch {
    return { commit: null, tree: null, dirty: null, dirty_paths: null };
  }
}

// Pointers every harness report carries once it embeds auditProvenance().
export const PROVENANCE_POINTERS = Object.freeze([
  '/git/commit',
  '/git/tree',
  '/git/dirty',
  '/source_fingerprint/production_source_sha256',
]);

// What a harness records at report creation so the acceptance manifest can
// bind its evidence to the checkout it ran from.
export function auditProvenance(root = DEFAULT_ROOT) {
  return { git: gitProvenance(root), source_fingerprint: productionSourceFingerprint(root) };
}

function isProductionInput(path) {
  if (!path || path.startsWith('../') || path.startsWith('/')) return false;
  if (EXCLUDED_PREFIXES.some((prefix) => path.startsWith(prefix))) return false;
  if (EXCLUDED_SUFFIXES.some((suffix) => path.endsWith(suffix))) return false;
  if (EXACT_INPUTS.has(path)) return true;
  if (/^src-tauri\/tauri\.[^/]+\.conf\.json5?$/.test(path)) return true;
  return INCLUDED_DIRS.some((prefix) => path === prefix || path.startsWith(`${prefix}/`));
}

function listByGit(root) {
  const output = execFileSync(
    'git',
    ['ls-files', '-z', '--cached', '--others', '--exclude-standard'],
    {
      cwd: root,
      encoding: 'buffer',
      stdio: ['ignore', 'pipe', 'ignore'],
    }
  );
  return output.toString('utf8').split('\0').filter(Boolean);
}

function walk(root, dir = root, out = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const abs = resolve(dir, entry.name);
    const rel = toRepoPath(root, abs);
    if (EXCLUDED_PREFIXES.some((prefix) => rel === prefix.slice(0, -1) || rel.startsWith(prefix)))
      continue;
    if (entry.isDirectory()) walk(root, abs, out);
    else if (entry.isFile()) out.push(rel);
  }
  return out;
}

export function productionInputFiles(root = DEFAULT_ROOT) {
  const absoluteRoot = resolve(root);
  let files;
  try {
    files = listByGit(absoluteRoot);
  } catch {
    files = walk(absoluteRoot);
  }
  return [...new Set(files.filter(isProductionInput))]
    .filter((file) => existsSync(resolve(absoluteRoot, file)))
    .sort();
}

export function productionSourceFingerprint(root = DEFAULT_ROOT) {
  const absoluteRoot = resolve(root);
  const files = productionInputFiles(absoluteRoot);
  const digest = createHash('sha256');
  for (const file of files) {
    const absolute = resolve(absoluteRoot, file);
    const stat = statSync(absolute);
    if (!stat.isFile()) continue;
    const bytes = readFileSync(absolute);
    digest.update(file);
    digest.update('\0');
    digest.update(String(bytes.length));
    digest.update('\0');
    digest.update(bytes);
    digest.update('\0');
  }
  return {
    schema_version: 1,
    algorithm: 'sha256-production-inputs-v1',
    production_source_sha256: digest.digest('hex'),
    file_count: files.length,
    includes_untracked_nonignored: true,
    excludes: ['docs/', 'scripts/', '.omx/', '.github/', 'target/', 'node_modules/', '.next/'],
    files,
  };
}

if (isMainModule(process.argv[1], import.meta.filename)) {
  const json = process.argv.includes('--json');
  const rootIndex = process.argv.indexOf('--root');
  const root = rootIndex === -1 ? DEFAULT_ROOT : resolve(process.argv[rootIndex + 1]);
  const fingerprint = productionSourceFingerprint(root);
  if (json) console.log(JSON.stringify(fingerprint, null, 2));
  else console.log(fingerprint.production_source_sha256);
}
