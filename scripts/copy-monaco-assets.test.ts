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
const REFUSAL = `[monaco-assets] monaco-editor ${VERSION} ships no`;

let project = '';

const at = (path: string) => join(project, path);

function put(path: string, content = '') {
  mkdirSync(dirname(at(path)), { recursive: true });
  writeFileSync(at(path), content);
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
  for (const file of VS_FILES) put(`${PACKAGE_VS}/${file}`);
});

afterEach(() => {
  rmSync(project, { recursive: true, force: true, maxRetries: 3 });
});

describe('copy-monaco-assets.mjs', () => {
  test('stages the editor, its five workers and both notices, then skips the next run', () => {
    const first = run();
    expect(first.status).toBe(0);
    expect(first.stdout).toContain(`copied monaco-editor ${VERSION}`);
    const staged = ['manifest.json', ...NOTICES, ...VS_FILES.map((file) => `vs/${file}`)];
    expect(staged.filter((file) => !existsSync(at(`${STAGE}/${file}`)))).toEqual([]);
    expect(JSON.parse(readFileSync(at(`${STAGE}/manifest.json`), 'utf8')).version).toBe(VERSION);

    expect(runSkips()).toBe(true);
  });

  for (const file of [workerBundle('json'), 'editor/editor.main.css']) {
    test(`re-copies when the staged ${file} has been deleted, then skips again`, () => {
      expect(run().status).toBe(0);
      const staged = at(`${STAGE}/vs/${file}`);
      rmSync(staged);

      const repaired = run();
      expect(repaired.status).toBe(0);
      expect(repaired.stdout).toContain(`copied monaco-editor ${VERSION}`);
      expect(existsSync(staged)).toBe(true);
      expect(runSkips()).toBe(true);
    });
  }

  for (const label of WORKER_LABELS) {
    test(`refuses a package with no ${label} worker bundle and leaves the stage alone`, () => {
      rmSync(at(`${PACKAGE_VS}/${workerBundle(label)}`));
      seedStage();

      const result = run();
      expect(result.status).not.toBe(0);
      expect(result.stderr).toContain(`${REFUSAL} worker bundle for ${label} in`);
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

  test('does not take editorWebWorkerMain-*.js for the editor worker', () => {
    rmSync(at(`${PACKAGE_VS}/${workerBundle('editor')}`));
    put(`${PACKAGE_VS}/assets/editorWebWorkerMain-abc.js`);

    const result = run();
    expect(result.status).not.toBe(0);
    expect(result.stderr).toContain(`${REFUSAL} worker bundle for editor in`);
  });

  test('counts an assets entry that is not a folder as every worker missing', () => {
    rmSync(at(`${PACKAGE_VS}/assets`), { recursive: true });
    put(`${PACKAGE_VS}/assets`, 'not a folder');

    const result = run();
    expect(result.status).not.toBe(0);
    expect(result.stderr).toContain(`${REFUSAL} worker bundle for ${WORKER_LABELS.join(', ')} in`);
  });

  for (const file of ['editor.main.js', 'editor.main.css']) {
    test(`refuses a package with no ${file} and leaves the stage alone`, () => {
      rmSync(at(`${PACKAGE_VS}/editor/${file}`));
      seedStage();

      const result = run();
      expect(result.status).not.toBe(0);
      expect(result.stderr).toContain(`${REFUSAL} editor/${file} at`);
      expect(readdirSync(at(STAGE))).toEqual(['marker.txt']);
    });
  }
});
