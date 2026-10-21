import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { spawnSync } from 'node:child_process';
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

// The script finds public/monaco-editor from the working directory and the
// monaco-editor package from its own location, so every case runs a copy of it
// in a throwaway project that holds a fake package, never in this repo's stage.
const SCRIPT = fileURLToPath(new URL('./copy-monaco-assets.mjs', import.meta.url));
const VERSION = '9.9.9';
const PACKAGE = 'node_modules/monaco-editor';
const PACKAGE_VS = `${PACKAGE}/min/vs`;
const STAGE = 'public/monaco-editor';
const MANIFEST = `${STAGE}/manifest.json`;
// The labels CI's payload check looks for in dist/monaco-editor/vs/assets.
const WORKER_LABELS = ['editor', 'css', 'html', 'json', 'ts'];
const NOTICES = ['LICENSE', 'ThirdPartyNotices.txt'];
const workerBundle = (label: string) => `assets/${label}.worker-abc.js`;
// What the fake package ships under min/vs, and so what the script stages under vs/.
const VS_FILES = [
  'loader.js',
  'editor/editor.main.js',
  'editor/editor.main.css',
  ...WORKER_LABELS.map((label) => workerBundle(label)),
];
// Also under min/vs, but never loaded at runtime, so never staged.
const TYPINGS = 'nls/lang/de.d.ts';
// What a run stages that the next run checks for before it skips the copy.
const STAGED = ['manifest.json', ...NOTICES, ...VS_FILES.map((file) => `vs/${file}`)];
const REFUSAL = `[monaco-assets] monaco-editor ${VERSION} ships no`;
// Each file the script requires of the package, in the order it checks them,
// and how its refusal names the missing file.
const REQUIRED: [file: string, refusal: string][] = [
  ['min/vs/loader.js', 'AMD build at'],
  ['min/vs/editor/editor.main.js', 'editor/editor.main.js at'],
  ['min/vs/editor/editor.main.css', 'editor/editor.main.css at'],
  ...WORKER_LABELS.map((label): [string, string] => [
    `min/vs/${workerBundle(label)}`,
    `worker bundle for ${label} in`,
  ]),
  ...NOTICES.map((file): [string, string] => [file, `${file} at`]),
];
// Assets that must not pass for a missing worker bundle.
const DECOYS: [label: string, decoy: string][] = [
  ['editor', 'editorWebWorkerMain-abc.js'],
  ['ts', 'ts.worker-abc.js.map'],
];

let project = '';

const at = (path: string) => join(project, path);

function put(path: string, content = '') {
  mkdirSync(dirname(at(path)), { recursive: true });
  writeFileSync(at(path), content);
}

function readManifest() {
  return JSON.parse(readFileSync(at(MANIFEST), 'utf8'));
}

// The test runner's own binary runs the script, so the test needs no `node` on
// PATH (`bun run assets:monaco` itself starts the script under Node).
function run() {
  return spawnSync(process.execPath, ['scripts/copy-monaco-assets.mjs'], {
    cwd: project,
    encoding: 'utf8',
  });
}

// A stage without a manifest cannot be skipped, so the script reaches its checks
// of the package; a refusal there must leave this stage exactly as it was.
function seedStage() {
  put(`${STAGE}/marker.txt`, 'seeded');
}

// Whether a run skipped the copy. A copy deletes the whole stage first, so a
// probe file left in it survives only a skip. (The "skipping copy" line is
// printed just before process.exit, which may cut short output to a pipe.)
function runSkips() {
  const probe = at(`${STAGE}/vs/skip-probe.txt`);
  writeFileSync(probe, '');
  return run().status === 0 && existsSync(probe);
}

beforeEach(() => {
  project = mkdtempSync(join(tmpdir(), 'monaco-assets-'));
  put('scripts/copy-monaco-assets.mjs', readFileSync(SCRIPT, 'utf8'));
  put(`${PACKAGE}/package.json`, JSON.stringify({ name: 'monaco-editor', version: VERSION }));
  for (const file of NOTICES) put(`${PACKAGE}/${file}`);
  for (const file of [...VS_FILES, TYPINGS]) put(`${PACKAGE_VS}/${file}`);
});

afterEach(() => {
  try {
    rmSync(project, { recursive: true, force: true, maxRetries: 3 });
  } catch {
    // Windows can hold a just-written file open past the retries; a temp folder
    // left behind must not fail the case that used it.
  }
});

describe('copy-monaco-assets.mjs', () => {
  test('stages the editor, its five workers and both notices, then skips the next run', () => {
    const first = run();
    expect(first.status).toBe(0);
    expect(first.stdout).toContain(`copied monaco-editor ${VERSION}`);
    expect(STAGED.filter((file) => !existsSync(at(`${STAGE}/${file}`)))).toEqual([]);
    expect(existsSync(at(`${STAGE}/vs/${TYPINGS}`))).toBe(false);
    expect(readManifest().version).toBe(VERSION);

    expect(runSkips()).toBe(true);
  });

  for (const file of STAGED) {
    test(`re-copies when the staged ${file} has been deleted, then skips again`, () => {
      expect(run().status).toBe(0);
      const staged = at(`${STAGE}/${file}`);
      rmSync(staged);

      const repaired = run();
      expect(repaired.status).toBe(0);
      expect(repaired.stdout).toContain(`copied monaco-editor ${VERSION}`);
      expect(existsSync(staged)).toBe(true);
      expect(runSkips()).toBe(true);
    });
  }

  test('replaces the whole stage when monaco-editor changes version', () => {
    expect(run().status).toBe(0);
    put(`${STAGE}/vs/stale.txt`);
    put(`${PACKAGE}/package.json`, JSON.stringify({ name: 'monaco-editor', version: '10.0.0' }));

    const upgraded = run();
    expect(upgraded.status).toBe(0);
    expect(upgraded.stdout).toContain('copied monaco-editor 10.0.0');
    expect(existsSync(at(`${STAGE}/vs/stale.txt`))).toBe(false);
    expect(readManifest().version).toBe('10.0.0');
  });

  test('re-copies a stage whose manifest records an older layout, then skips again', () => {
    expect(run().status).toBe(0);
    const manifest = readManifest();
    writeFileSync(at(MANIFEST), JSON.stringify({ ...manifest, layout: manifest.layout - 1 }));

    const recopied = run();
    expect(recopied.status).toBe(0);
    expect(recopied.stdout).toContain(`copied monaco-editor ${VERSION}`);
    expect(readManifest()).toEqual(manifest);
    expect(runSkips()).toBe(true);
  });

  for (const content of ['{', 'null']) {
    test(`re-copies over a manifest that reads ${content}`, () => {
      expect(run().status).toBe(0);
      writeFileSync(at(MANIFEST), content);

      const recopied = run();
      expect(recopied.status).toBe(0);
      expect(recopied.stdout).toContain(`copied monaco-editor ${VERSION}`);
      expect(readManifest().version).toBe(VERSION);
    });
  }

  for (const [file, refusal] of REQUIRED) {
    test(`refuses a package with no ${file} and leaves the stage alone`, () => {
      rmSync(at(`${PACKAGE}/${file}`));
      seedStage();

      const result = run();
      expect(result.status).not.toBe(0);
      expect(result.stderr).toContain(`${REFUSAL} ${refusal}`);
      expect(readdirSync(at(STAGE))).toEqual(['marker.txt']);
    });
  }

  test('names every missing worker, comma separated', () => {
    rmSync(at(`${PACKAGE_VS}/${workerBundle('css')}`));
    rmSync(at(`${PACKAGE_VS}/${workerBundle('ts')}`));

    const result = run();
    expect(result.status).not.toBe(0);
    expect(result.stderr).toContain(`${REFUSAL} worker bundle for css, ts in`);
  });

  for (const [label, decoy] of DECOYS) {
    test(`does not take ${decoy} for the ${label} worker bundle`, () => {
      rmSync(at(`${PACKAGE_VS}/${workerBundle(label)}`));
      put(`${PACKAGE_VS}/assets/${decoy}`);

      const result = run();
      expect(result.status).not.toBe(0);
      expect(result.stderr).toContain(`${REFUSAL} worker bundle for ${label} in`);
    });
  }

  test('counts an assets entry that is not a folder as every worker missing', () => {
    rmSync(at(`${PACKAGE_VS}/assets`), { recursive: true });
    put(`${PACKAGE_VS}/assets`, 'not a folder');

    const result = run();
    expect(result.status).not.toBe(0);
    expect(result.stderr).toContain(`${REFUSAL} worker bundle for ${WORKER_LABELS.join(', ')} in`);
  });
});
