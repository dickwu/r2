import { describe, expect, test } from 'bun:test';
import { join } from 'node:path';
import { FFMPEG_BUILDS, requestedTriples, sidecarPath } from './fetch-ffmpeg';

describe('fetch-ffmpeg', () => {
  test('pins a sha256-checked build for every shipped platform', () => {
    for (const triple of [
      'aarch64-apple-darwin',
      'x86_64-apple-darwin',
      'x86_64-unknown-linux-gnu',
      'x86_64-pc-windows-msvc',
    ]) {
      const build = FFMPEG_BUILDS[triple];
      expect(build).not.toBeUndefined();
      expect(build.sha256).toMatch(/^[0-9a-f]{64}$/);
      expect(build.url.startsWith('https://')).toBe(true);
      expect(build.version).toContain('9.0.2');
    }
  });

  test('sidecar names carry the triple and .exe on Windows', () => {
    expect(sidecarPath('aarch64-apple-darwin', 'b')).toBe(
      join('b', 'r2-ffmpeg-aarch64-apple-darwin')
    );
    expect(sidecarPath('x86_64-pc-windows-msvc', 'b')).toBe(
      join('b', 'r2-ffmpeg-x86_64-pc-windows-msvc.exe')
    );
  });

  test('takes the host plus every --target, without duplicates', () => {
    expect(requestedTriples(['--target', 'x86_64-apple-darwin'], 'aarch64-apple-darwin')).toEqual([
      'aarch64-apple-darwin',
      'x86_64-apple-darwin',
    ]);
    expect(requestedTriples(['--target=aarch64-apple-darwin'], 'aarch64-apple-darwin')).toEqual([
      'aarch64-apple-darwin',
    ]);
    expect(requestedTriples([], 'h', 'x86_64-pc-windows-msvc')).toEqual(['x86_64-pc-windows-msvc']);
  });
});
