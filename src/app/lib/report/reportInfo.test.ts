import { describe, expect, mock, test } from 'bun:test';

mock.module('@tauri-apps/api/app', () => ({
  getVersion: () => Promise.resolve('0.3.2'),
}));

mock.module('@tauri-apps/plugin-os', () => ({
  platform: () => 'macos',
  version: () => '15.6',
  arch: () => 'aarch64',
}));

// Import after the mocks are registered so the lazy imports resolve to them.
const { buildReportInfo, fallbackReportInfo } = await import('./reportInfo');

describe('fallbackReportInfo', () => {
  test('carries the context and a timestamp even without a runtime', () => {
    const info = fallbackReportInfo({ provider: 'r2', theme: 'dark' });

    expect(info.provider).toBe('r2');
    expect(info.theme).toBe('dark');
    expect(info.appVersion).toBe('');
    expect(info.os).toBe('');
    expect(new Date(info.capturedAt).toISOString()).toBe(info.capturedAt);
  });
});

describe('buildReportInfo', () => {
  test('composes version and os from the Tauri runtime', async () => {
    const info = await buildReportInfo({ provider: 'aws', theme: 'light' });

    expect(info.appVersion).toBe('0.3.2');
    expect(info.os).toBe('macos 15.6 (aarch64)');
    expect(info.provider).toBe('aws');
    expect(info.theme).toBe('light');
  });
});
