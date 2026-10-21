import { describe, expect, spyOn, test } from 'bun:test';
import { loader } from '@monaco-editor/react';
import { configureMonacoLoader } from './monacoAssets';

describe('configureMonacoLoader', () => {
  test('resolves the staged vs/ folder under /monaco-editor/ for every origin Tauri uses', () => {
    for (const origin of [
      'tauri://localhost/',
      'http://tauri.localhost/',
      'http://localhost:3000/',
    ]) {
      expect(configureMonacoLoader(origin)).toBe(`${origin}monaco-editor/vs`);
    }
  });

  test('resolves against the origin, not the current route', () => {
    // A relative path would land under the route (…/deep/monaco-editor/vs).
    for (const origin of ['tauri://localhost', 'http://tauri.localhost']) {
      expect(configureMonacoLoader(`${origin}/deep/route?file=notes.txt#L3`)).toBe(
        `${origin}/monaco-editor/vs`
      );
    }
  });

  test('hands the loader the staged vs/ folder, where it loads loader.js from', () => {
    // A spy, not an injected <script>: the loader injects loader.js only on the
    // first init() in a process, so that check could not be repeated.
    const config = spyOn(loader, 'config');
    try {
      const vs = configureMonacoLoader('tauri://localhost/');
      expect(config.mock.calls).toEqual([[{ paths: { vs } }]]);
    } finally {
      config.mockRestore();
    }
  });
});
