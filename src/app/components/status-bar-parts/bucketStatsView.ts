import { formatBytes } from '@/app/utils/formatBytes';
import type { BucketSummary } from '@/app/lib/r2cache';

export interface BucketStatsView {
  text: string;
  /** True while the background sync is still counting the bucket up. */
  live: boolean;
}

/**
 * Decide what the status-bar bucket summary should show.
 *
 * - While the background sync crawls a (big) bucket, surface the live running
 *   totals from the sync events so numbers appear immediately instead of
 *   after the full fetch + index completes.
 * - Once idle, show the cached summary; `~` marks partial (lazy-browsed)
 *   data that doesn't cover the whole bucket yet.
 */
export function computeBucketStatsView(args: {
  isSyncRunning: boolean;
  objectsFetched: number;
  bytesFetched: number;
  summary: BucketSummary | null | undefined;
}): BucketStatsView | null {
  const { isSyncRunning, objectsFetched, bytesFetched, summary } = args;

  if (isSyncRunning) {
    if (objectsFetched <= 0) return null;
    return {
      text: `Bucket: ${objectsFetched.toLocaleString()} files · ${formatBytes(bytesFetched)}…`,
      live: true,
    };
  }

  if (!summary) return null;

  // Nothing cached and no finished sync — hide rather than claim "0 files".
  if (!summary.isComplete && summary.totalFiles <= 0) return null;

  const prefix = summary.isComplete ? '' : '~';
  return {
    text: `Bucket: ${prefix}${summary.totalFiles.toLocaleString()} files · ${formatBytes(summary.totalSize)}`,
    live: false,
  };
}
