#!/usr/bin/env node
// Stages the file-viewer runtime payload (Workers, WASM, fonts, vendor bundles)
// into public/file-viewer so Next's static export ships it in dist/ and the
// Tauri bundle serves it at <origin>/file-viewer/. The copy is ~200 MB, so it
// is skipped when the manifest already matches the installed CLI version.
import { existsSync, readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';

const require = createRequire(import.meta.url);
const targetDir = resolve(process.env.FILE_VIEWER_PUBLIC_DIR || 'public/file-viewer');
const { version } = require('file-viewer-copy-assets/package.json');
const manifestPath = resolve(targetDir, 'flyfish-viewer-manifest.json');

if (existsSync(manifestPath)) {
  try {
    const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
    if (manifest.version === version) {
      console.log(`[file-viewer-assets] ${targetDir} already at ${version}, skipping copy`);
      process.exit(0);
    }
  } catch {
    // Unreadable manifest: fall through and recopy.
  }
}

const { copyFileViewerAssets } = await import('file-viewer-copy-assets');
const result = await copyFileViewerAssets({ targetDir, clean: true, confirmClean: true });
if (result.validation.missingOptional.length) {
  console.warn(
    `[file-viewer-assets] optional assets missing: ${result.validation.missingOptional
      .map((asset) => asset.relativePath)
      .join(', ')}`
  );
}
console.log(`[file-viewer-assets] copied ${version} payload to ${result.targetDir}`);
