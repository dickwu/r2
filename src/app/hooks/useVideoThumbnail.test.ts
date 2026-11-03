import { describe, expect, mock, test } from 'bun:test';

type Reply = () => Promise<{ dataUrl: string; fromCache: boolean }>;
let reply: Reply = () =>
  Promise.resolve({ dataUrl: 'data:image/webp;base64,AAAA', fromCache: false });
const calls: { command: string; args: Record<string, unknown> }[] = [];
// Keep the real exports: bun module mocks are process-wide, and Tauri's event
// module (imported by other test files) needs transformCallback from core.
const tauriCore = await import('@tauri-apps/api/core');
mock.module('@tauri-apps/api/core', () => ({
  ...tauriCore,
  invoke: (command: string, args: Record<string, unknown>) => {
    calls.push({ command, args });
    return reply();
  },
}));

const { getVideoThumbnailSrc, isFfmpegMissing } = await import('./useVideoThumbnail');
const { clearThumbnailMemory, forgetThumbnails, getThumbnailSrc, thumbnailFailure } =
  await import('./useThumbnail');

const object = (key: string, size = 10) => ({
  accountId: 'acct',
  bucket: 'media',
  key,
  size,
  lastModified: '2026-11-01T00:00:00Z',
});

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

  test('returns the rendered frame through the cached command', async () => {
    calls.length = 0;
    reply = () => Promise.resolve({ dataUrl: 'data:image/webp;base64,AAAA', fromCache: true });
    expect(await getVideoThumbnailSrc('https://h/b/c.mp4', object('c.mp4'))).toBe(
      'data:image/webp;base64,AAAA'
    );
    expect(isFfmpegMissing()).toBe(false);
    expect(calls).toHaveLength(1);
    expect(calls[0].command).toBe('get_thumbnail');
    expect(calls[0].args).toMatchObject({ kind: 'video', object: object('c.mp4') });
  });
});

describe('session thumbnails', () => {
  test('a re-signed URL of the same object is not fetched again', async () => {
    calls.length = 0;
    reply = () => Promise.resolve({ dataUrl: 'data:image/webp;base64,BBBB', fromCache: false });
    await getThumbnailSrc('image', 'https://h/b/p.jpg?sig=1', object('p.jpg'));
    await getThumbnailSrc('image', 'https://h/b/p.jpg?sig=2', object('p.jpg'));
    expect(calls).toHaveLength(1);
  });

  test('a changed object is fetched again', async () => {
    calls.length = 0;
    await getThumbnailSrc('image', 'https://h/b/q.jpg', object('q.jpg', 1));
    await getThumbnailSrc('image', 'https://h/b/q.jpg', object('q.jpg', 2));
    expect(calls).toHaveLength(2);
  });

  test('forgetting an object drops every version of it, and only it', async () => {
    await getThumbnailSrc('image', 'https://h/b/r.jpg', object('r.jpg'));
    await getThumbnailSrc('image', 'https://h/b/s.jpg', object('s.jpg'));
    calls.length = 0;
    expect(forgetThumbnails('acct', 'media', ['r.jpg'])).toBe(1);
    await getThumbnailSrc('image', 'https://h/b/r.jpg', object('r.jpg'));
    await getThumbnailSrc('image', 'https://h/b/s.jpg', object('s.jpg'));
    expect(calls).toHaveLength(1);
    expect(calls[0].args).toMatchObject({ object: { key: 'r.jpg' } });
  });

  test('clearing empties the session layer and remembers no failures', async () => {
    reply = () => Promise.reject('image_too_large');
    await getThumbnailSrc('image', 'https://h/b/big.jpg', object('big.jpg'));
    expect(thumbnailFailure('image', 'https://h/b/big.jpg', object('big.jpg'))).toBe(
      'image_too_large'
    );
    clearThumbnailMemory();
    expect(thumbnailFailure('image', 'https://h/b/big.jpg', object('big.jpg'))).toBeUndefined();
    calls.length = 0;
    reply = () => Promise.resolve({ dataUrl: 'data:image/webp;base64,CCCC', fromCache: false });
    await getThumbnailSrc('image', 'https://h/b/s.jpg', object('s.jpg'));
    expect(calls).toHaveLength(1);
  });
});
