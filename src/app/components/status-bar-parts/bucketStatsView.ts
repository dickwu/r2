import { formatBytes } from '@/app/utils/formatBytes';
import { t as i18nT } from '@/i18n';
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
      text: i18nT('status.bucketLive', {
        count: objectsFetched,
        files: objectsFetched.toLocaleString(),
        size: formatBytes(bytesFetched),
      }),
      live: true,
    };
  }

  if (!summary) return null;

  // Nothing cached and no finished sync — hide rather than claim "0 files".
  if (!summary.isComplete && summary.totalFiles <= 0) return null;

  const prefix = summary.isComplete ? '' : '~';
  return {
    text: i18nT('status.bucket', {
      count: summary.totalFiles,
      files: `${prefix}${summary.totalFiles.toLocaleString()}`,
      size: formatBytes(summary.totalSize),
    }),
    live: false,
  };
}
