#!/usr/bin/env node
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, statSync, readdirSync } from 'node:fs';
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

if (process.argv[1] && resolve(process.argv[1]) === resolve(import.meta.filename)) {
  const json = process.argv.includes('--json');
  const rootIndex = process.argv.indexOf('--root');
  const root = rootIndex === -1 ? DEFAULT_ROOT : resolve(process.argv[rootIndex + 1]);
  const fingerprint = productionSourceFingerprint(root);
  if (json) console.log(JSON.stringify(fingerprint, null, 2));
  else console.log(fingerprint.production_source_sha256);
}
