#!/usr/bin/env node
// Stages Monaco's AMD build (monaco-editor/min/vs) into public/monaco-editor/vs
// so Next's static export ships it in dist/ and the Tauri bundle serves it at
// <origin>/monaco-editor/vs. Without it @monaco-editor/react's loader pulls its
// own default Monaco from jsdelivr, which never loads offline and is not the
// version package.json pins. The copy is skipped when the manifest already
// matches the installed monaco-editor version.
import { cpSync, existsSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';

const require = createRequire(import.meta.url);
const targetDir = resolve('public/monaco-editor');
const manifestPath = resolve(targetDir, 'manifest.json');

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
    if (manifest.version === version) {
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

rmSync(targetDir, { recursive: true, force: true });
cpSync(sourceDir, resolve(targetDir, 'vs'), {
  recursive: true,
  // nls/lang/*.d.ts are never loaded at runtime, and tsc's **/*.ts include
  // would otherwise type-check them.
  filter: (source) => !source.endsWith('.d.ts'),
});
// Written last, so an interrupted copy is redone on the next run.
writeFileSync(manifestPath, `${JSON.stringify({ version }, null, 2)}\n`);
console.log(`[monaco-assets] copied monaco-editor ${version} min/vs to ${targetDir}/vs`);
