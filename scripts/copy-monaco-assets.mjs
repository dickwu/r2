#!/usr/bin/env node
// Stages Monaco's AMD build (monaco-editor/min/vs) into public/monaco-editor/vs
// so Next's static export ships it in dist/ and the Tauri bundle serves it at
// <origin>/monaco-editor/vs. Without it @monaco-editor/react's loader pulls its
// own default Monaco from jsdelivr, which never loads offline and is not the
// version package.json pins. The package's license files are staged beside
// vs/. The copy is skipped when the manifest already matches the installed
// monaco-editor version and this script's layout, and the loader, editor.main,
// the license files and the vs/assets/<label>.worker-*.js worker bundles are
// still in place.
import {
  copyFileSync,
  cpSync,
  existsSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';

const require = createRequire(import.meta.url);
const targetDir = resolve('public/monaco-editor');
const manifestPath = resolve(targetDir, 'manifest.json');
// Every installer redistributes Monaco (MIT, bundling TypeScript under
// Apache-2.0 among others), but min/vs carries neither notice.
const LICENSE_FILES = ['LICENSE', 'ThirdPartyNotices.txt'];
// Bump whenever what this script stages changes: a manifest whose version
// matches but whose layout does not is re-copied, not skipped.
const LAYOUT = 2;
// The loader requires editor.main.js, which links editor.main.css.
const EDITOR_MAIN_FILES = ['editor/editor.main.js', 'editor/editor.main.css'];
// editor.main starts its Workers from the hashed vs/assets/<label>.worker-*.js
// bundles; the CI payload check lists the same labels.
const WORKER_LABELS = ['editor', 'css', 'html', 'json', 'ts'];

// The WORKER_LABELS with no <label>.worker-*.js bundle in assetsDir. The names
// carry a content hash, so they match on label and extension; a path that is
// not a readable folder lacks every one.
function missingWorkers(assetsDir) {
  let assets;
  try {
    assets = readdirSync(assetsDir);
  } catch {
    return [...WORKER_LABELS];
  }
  return WORKER_LABELS.filter(
    (label) => !assets.some((name) => name.startsWith(`${label}.worker-`) && name.endsWith('.js'))
  );
}

// monaco-editor's exports map hides its package.json from require(), so find
// the package directory the way Node's resolver walks node_modules.
const packageDir = require.resolve
  .paths('monaco-editor')
  .map((dir) => resolve(dir, 'monaco-editor'))
  .find((dir) => existsSync(resolve(dir, 'package.json')));
if (!packageDir) {
  throw new Error('[monaco-assets] monaco-editor is not installed; run bun install');
}
const { version } = JSON.parse(readFileSync(resolve(packageDir, 'package.json'), 'utf8'));

if (existsSync(manifestPath)) {
  try {
    const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
    const staged = [
      'vs/loader.js',
      ...EDITOR_MAIN_FILES.map((file) => `vs/${file}`),
      ...LICENSE_FILES,
    ].every((file) => existsSync(resolve(targetDir, file)));
    const workersStaged = missingWorkers(resolve(targetDir, 'vs/assets')).length === 0;
    if (manifest.version === version && manifest.layout === LAYOUT && staged && workersStaged) {
      console.log(`[monaco-assets] ${targetDir} already at ${version}, skipping copy`);
      process.exit(0);
    }
  } catch {
    // Unreadable manifest: fall through and recopy.
  }
}

// Monaco deprecated the AMD build in 0.53; fail the build rather than ship an
// editor that spins forever once an upgrade stops including it.
const sourceDir = resolve(packageDir, 'min/vs');
if (!existsSync(resolve(sourceDir, 'loader.js'))) {
  throw new Error(
    `[monaco-assets] monaco-editor ${version} ships no AMD build at ${sourceDir}; ` +
      "@monaco-editor/react's loader needs min/vs/loader.js"
  );
}
// CI's dist check requires both, but release.yml checks no payload.
const missingEditorFiles = EDITOR_MAIN_FILES.filter(
  (file) => !existsSync(resolve(sourceDir, file))
);
if (missingEditorFiles.length) {
  throw new Error(
    `[monaco-assets] monaco-editor ${version} ships no ${missingEditorFiles.join(' or ')} ` +
      `at ${sourceDir}; the editor starts from editor.main.js, which links editor.main.css`
  );
}
// A Monaco that renames or drops a worker would otherwise be staged without it
// on every run, and release.yml checks no payload.
const sourceAssetsDir = resolve(sourceDir, 'assets');
const missingSourceWorkers = missingWorkers(sourceAssetsDir);
if (missingSourceWorkers.length) {
  throw new Error(
    `[monaco-assets] monaco-editor ${version} ships no worker bundle for ` +
      `${missingSourceWorkers.join(', ')} in ${sourceAssetsDir}; ` +
      'editor.main starts its Workers from them'
  );
}
const missingLicenses = LICENSE_FILES.filter((file) => !existsSync(resolve(packageDir, file)));
if (missingLicenses.length) {
  throw new Error(
    `[monaco-assets] monaco-editor ${version} ships no ${missingLicenses.join(' or ')} ` +
      `at ${packageDir}; the app bundles Monaco, so it must ship its license notices too`
  );
}

// maxRetries: on Windows, rmSync fails with EBUSY while next dev holds a staged
// file open.
rmSync(targetDir, { recursive: true, force: true, maxRetries: 3 });
cpSync(sourceDir, resolve(targetDir, 'vs'), {
  recursive: true,
  // nls/lang/*.d.ts are never loaded at runtime, and tsc's **/*.ts include
  // would otherwise type-check them.
  filter: (source) => !source.endsWith('.d.ts'),
});
for (const file of LICENSE_FILES) {
  copyFileSync(resolve(packageDir, file), resolve(targetDir, file));
}
// Written last, so an interrupted copy is redone on the next run.
writeFileSync(manifestPath, `${JSON.stringify({ version, layout: LAYOUT }, null, 2)}\n`);
console.log(
  `[monaco-assets] copied monaco-editor ${version} min/vs and license files to ${targetDir}`
);
