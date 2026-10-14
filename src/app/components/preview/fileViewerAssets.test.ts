import { describe, expect, test } from 'bun:test';
import {
  resolveFileViewerHangulWorkerUrl,
  resolveFileViewerIworkWorkerUrl,
  resolveFileViewerWordPerfectWasmUrl,
} from '@file-viewer/core/assets';
import { getDefaultFullAssetBaseUrl } from '@file-viewer/preset-all';
import { configureFileViewerAssetBase } from './fileViewerAssets';

describe('configureFileViewerAssetBase', () => {
  test('resolves the staged payload under /file-viewer/ for every origin Tauri uses', () => {
    for (const origin of [
      'tauri://localhost/',
      'http://tauri.localhost/',
      'http://localhost:3000/',
    ]) {
      const base = configureFileViewerAssetBase(origin);
      expect(base).toBe(`${origin}file-viewer/`);
      expect(getDefaultFullAssetBaseUrl()).toBe(base);
    }
  });

  test('renderers outside the full preset merge resolve their Workers from the same base', () => {
    const base = configureFileViewerAssetBase('tauri://localhost/');
    const documentBase = 'tauri://localhost/';
    expect(resolveFileViewerIworkWorkerUrl(undefined, documentBase)).toBe(
      `${base}vendor/iwork/iwork.worker.js`
    );
    expect(resolveFileViewerHangulWorkerUrl(undefined, documentBase)).toBe(
      `${base}vendor/hangul/hangul.worker.js`
    );
    expect(
      resolveFileViewerWordPerfectWasmUrl(undefined, documentBase).startsWith(
        `${base}vendor/wordperfect/`
      )
    ).toBe(true);
  });
});
