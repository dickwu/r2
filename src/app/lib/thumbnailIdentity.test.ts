import { describe, expect, test } from 'bun:test';
import {
  MAX_PROACTIVE_REFRESH,
  memoryKeyIsObject,
  planThumbnailRefresh,
  thumbnailMemoryKey,
} from '@/app/lib/thumbnailIdentity';
import { cacheUsagePercent } from '@/app/lib/thumbnailCacheApi';

const obj = { accountId: 'a', bucket: 'b', key: 'x/y.jpg', size: 5, lastModified: 't1' };

describe('thumbnailMemoryKey', () => {
  test('an object ignores the URL; a changed object gets a new key', () => {
    const one = thumbnailMemoryKey('image', 'https://h/x/y.jpg?sig=1', obj);
    expect(thumbnailMemoryKey('image', 'https://other/x/y.jpg?sig=2', obj)).toBe(one);
    expect(thumbnailMemoryKey('image', 'https://h/x/y.jpg', { ...obj, size: 6 })).not.toBe(one);
    expect(
      thumbnailMemoryKey('image', 'https://h/x/y.jpg', { ...obj, lastModified: 't2' })
    ).not.toBe(one);
    expect(thumbnailMemoryKey('video', 'https://h/x/y.jpg', obj)).not.toBe(one);
  });

  test('without an object the URL minus its query is the key', () => {
    expect(thumbnailMemoryKey('video', 'https://h/v.mp4?sig=1')).toBe(
      thumbnailMemoryKey('video', 'https://h/v.mp4?sig=2')
    );
  });

  test('fields cannot run into each other', () => {
    const a = thumbnailMemoryKey('image', '', { ...obj, bucket: 'b|c', key: 'd' });
    const b = thumbnailMemoryKey('image', '', { ...obj, bucket: 'b', key: 'c|d' });
    expect(a).not.toBe(b);
  });

  test('memoryKeyIsObject matches any version of the key in that bucket only', () => {
    const keys = new Set(['x/y.jpg']);
    expect(memoryKeyIsObject(thumbnailMemoryKey('image', '', obj), 'a', 'b', keys)).toBe(true);
    expect(
      memoryKeyIsObject(thumbnailMemoryKey('video', '', { ...obj, size: 9 }), 'a', 'b', keys)
    ).toBe(true);
    expect(memoryKeyIsObject(thumbnailMemoryKey('image', '', obj), 'a', 'other', keys)).toBe(false);
    expect(
      memoryKeyIsObject(thumbnailMemoryKey('image', 'https://h/x/y.jpg'), 'a', 'b', keys)
    ).toBe(false);
  });
});

describe('planThumbnailRefresh', () => {
  const event = {
    accountId: 'a',
    bucket: 'b',
    changed: [
      { key: 'new.jpg', kind: 'image' as const, size: 9, lastModified: '2026-11-02T00:00:00Z' },
    ],
    removed: ['gone.mp4'],
  };

  test('regenerates changed objects and forgets removed ones in the open bucket', () => {
    const plan = planThumbnailRefresh(event, { accountId: 'a', bucket: 'b' });
    expect(plan.forget).toEqual(['new.jpg', 'gone.mp4']);
    expect(plan.regenerate).toEqual([
      {
        kind: 'image',
        accountId: 'a',
        bucket: 'b',
        key: 'new.jpg',
        size: 9,
        lastModified: '2026-11-02T00:00:00Z',
      },
    ]);
  });

  test('ignores other buckets and caps how much is remade at once', () => {
    expect(planThumbnailRefresh(event, { accountId: 'a', bucket: 'c' }).regenerate).toEqual([]);
    expect(planThumbnailRefresh(event, null).forget).toEqual([]);
    const many = {
      ...event,
      changed: Array.from({ length: 100 }, (_, i) => ({ key: `${i}.jpg`, kind: 'image' as const })),
    };
    expect(planThumbnailRefresh(many, { accountId: 'a', bucket: 'b' }).regenerate).toHaveLength(
      MAX_PROACTIVE_REFRESH
    );
  });
});

describe('cacheUsagePercent', () => {
  test('is bounded and safe with no limit', () => {
    expect(cacheUsagePercent({ bytes: 50, maxBytes: 200 })).toBe(25);
    expect(cacheUsagePercent({ bytes: 500, maxBytes: 200 })).toBe(100);
    expect(cacheUsagePercent({ bytes: 5, maxBytes: 0 })).toBe(0);
  });
});
