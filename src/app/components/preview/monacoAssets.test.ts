import { afterEach, describe, expect, test } from 'bun:test';
import { loader } from '@monaco-editor/react';
import { configureMonacoLoader } from './monacoAssets';

const BROWSER_GLOBALS = ['window', 'document'] as const;
const originalGlobals = BROWSER_GLOBALS.map(
  (key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const
);
afterEach(() => {
  for (const [key, descriptor] of originalGlobals) {
    if (descriptor) Object.defineProperty(globalThis, key, descriptor);
    else Reflect.deleteProperty(globalThis, key);
  }
});

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

  test('the loader injects loader.js from the staged copy instead of its jsdelivr default', () => {
    const injected: { src?: string }[] = [];
    Object.defineProperty(globalThis, 'window', { configurable: true, value: {} });
    Object.defineProperty(globalThis, 'document', {
      configurable: true,
      value: {
        createElement: () => ({}),
        body: { appendChild: (script: { src?: string }) => injected.push(script) },
      },
    });

    const vs = configureMonacoLoader('tauri://localhost/');
    // Never settles here: nothing fires the injected script's onload.
    void loader.init();
    expect(injected.map((script) => script.src)).toEqual([`${vs}/loader.js`]);
  });
});
