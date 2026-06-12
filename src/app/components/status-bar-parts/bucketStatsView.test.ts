import { describe, expect, test } from 'bun:test';
import { computeBucketStatsView } from './bucketStatsView';
import type { BucketSummary } from '@/app/lib/r2cache';

function summary(overrides: Partial<BucketSummary> = {}): BucketSummary {
  return {
    totalFiles: 1234,
    totalSize: 5 * 1024 * 1024,
    lastModified: '2026-06-01T00:00:00Z',
    isComplete: true,
    ...overrides,
  };
}

describe('computeBucketStatsView', () => {
  test('shows live running totals while background sync is counting', () => {
    const view = computeBucketStatsView({
      isSyncRunning: true,
      objectsFetched: 250_000,
      bytesFetched: 1024 * 1024 * 1024,
      summary: undefined,
    });
    expect(view).toEqual({
      text: `Bucket: ${(250_000).toLocaleString()} files · 1 GB…`,
      live: true,
    });
  });

  test('hides during sync until the first page of objects arrives', () => {
    const view = computeBucketStatsView({
      isSyncRunning: true,
      objectsFetched: 0,
      bytesFetched: 0,
      summary: summary(),
    });
    expect(view).toBeNull();
  });

  test('hides when idle with no summary loaded yet', () => {
    expect(
      computeBucketStatsView({
        isSyncRunning: false,
        objectsFetched: 0,
        bytesFetched: 0,
        summary: undefined,
      })
    ).toBeNull();
  });

  test('shows exact totals from a complete summary', () => {
    const view = computeBucketStatsView({
      isSyncRunning: false,
      objectsFetched: 0,
      bytesFetched: 0,
      summary: summary(),
    });
    expect(view).toEqual({
      text: `Bucket: ${(1234).toLocaleString()} files · 5 MB`,
      live: false,
    });
  });

  test('marks partial (lazy-browsed) data with a ~ prefix', () => {
    const view = computeBucketStatsView({
      isSyncRunning: false,
      objectsFetched: 0,
      bytesFetched: 0,
      summary: summary({ isComplete: false, totalFiles: 42, totalSize: 2048 }),
    });
    expect(view).toEqual({
      text: `Bucket: ~${(42).toLocaleString()} files · 2 KB`,
      live: false,
    });
  });

  test('hides partial data with zero files instead of claiming an empty bucket', () => {
    expect(
      computeBucketStatsView({
        isSyncRunning: false,
        objectsFetched: 0,
        bytesFetched: 0,
        summary: summary({ isComplete: false, totalFiles: 0, totalSize: 0 }),
      })
    ).toBeNull();
  });

  test('a genuinely empty bucket (complete) is shown as 0 files', () => {
    const view = computeBucketStatsView({
      isSyncRunning: false,
      objectsFetched: 0,
      bytesFetched: 0,
      summary: summary({ totalFiles: 0, totalSize: 0, lastModified: null }),
    });
    expect(view).toEqual({
      text: 'Bucket: 0 files · 0 B',
      live: false,
    });
  });
});
