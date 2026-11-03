import { describe, expect, mock, test } from 'bun:test';

let reply: () => Promise<string> = () => Promise.resolve('data:image/jpeg;base64,AAAA');
// Keep the real exports: bun module mocks are process-wide, and Tauri's event
// module (imported by other test files) needs transformCallback from core.
const tauriCore = await import('@tauri-apps/api/core');
mock.module('@tauri-apps/api/core', () => ({
  ...tauriCore,
  invoke: (command: string) => {
    expect(command).toBe('generate_video_thumbnail');
    return reply();
  },
}));

const { getVideoThumbnailSrc, isFfmpegMissing } = await import('./useVideoThumbnail');

describe('getVideoThumbnailSrc', () => {
  test('reports a missing ffmpeg so the tile can say so', async () => {
    reply = () => Promise.reject('ffmpeg_not_found');
    expect(await getVideoThumbnailSrc('https://h/b/a.mp4?sig=1')).toBeNull();
    expect(isFfmpegMissing()).toBe(true);
  });

  test('other failures are not blamed on ffmpeg', async () => {
    reply = () => Promise.reject('ffmpeg failed: 403 Forbidden');
    expect(await getVideoThumbnailSrc('https://h/b/b.mp4')).toBeNull();
    expect(isFfmpegMissing()).toBe(false);
  });

  test('returns the rendered frame', async () => {
    reply = () => Promise.resolve('data:image/jpeg;base64,AAAA');
    expect(await getVideoThumbnailSrc('https://h/b/c.mp4')).toBe('data:image/jpeg;base64,AAAA');
    expect(isFfmpegMissing()).toBe(false);
  });
});
