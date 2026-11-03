import { describe, expect, test } from 'bun:test';
import {
  mediaFragmentUrl,
  thumbnailCacheKey,
  VideoThumbnailCache,
} from '@/app/lib/videoThumbnailCache';

describe('thumbnailCacheKey', () => {
  test('drops the signature so a re-signed URL hits the same entry', () => {
    const a = 'https://acct.r2.cloudflarestorage.com/file/B-Roll%20Gd(1).mp4?X-Amz-Signature=aaa';
    const b = 'https://acct.r2.cloudflarestorage.com/file/B-Roll%20Gd(1).mp4?X-Amz-Signature=bbb';
    expect(thumbnailCacheKey(a)).toBe(thumbnailCacheKey(b));
    expect(thumbnailCacheKey(a)).toBe(
      'https://acct.r2.cloudflarestorage.com/file/B-Roll%20Gd(1).mp4'
    );
  });

  test('keeps hosts apart and leaves local paths alone', () => {
    expect(thumbnailCacheKey('https://cdn.example.com/v.mp4')).not.toBe(
      thumbnailCacheKey('https://other.example.com/v.mp4')
    );
    expect(thumbnailCacheKey('/Users/me/v.mp4')).toBe('/Users/me/v.mp4');
  });
});

describe('mediaFragmentUrl', () => {
  test('appends the 1 s fragment, replacing an existing one', () => {
    expect(mediaFragmentUrl('https://x/v.mp4?sig=1')).toBe('https://x/v.mp4?sig=1#t=1');
    expect(mediaFragmentUrl('https://x/v.mp4#t=5')).toBe('https://x/v.mp4#t=1');
  });
});

describe('VideoThumbnailCache', () => {
  test('keeps successes and expires failures', () => {
    const cache = new VideoThumbnailCache(1000);
    cache.set('https://x/a.mp4?s=1', 'data:image/jpeg;base64,AAA', 0);
    cache.set('https://x/b.mp4', null, 0);

    expect(cache.get('https://x/a.mp4?s=2', 10_000)).toBe('data:image/jpeg;base64,AAA');
    expect(cache.get('https://x/b.mp4', 500)).toBeNull();
    expect(cache.get('https://x/b.mp4', 1000)).toBeUndefined();
    expect(cache.get('https://x/c.mp4', 0)).toBeUndefined();
  });
});
