'use client';

import { useMemo } from 'react';
import { useQuery } from '@tanstack/react-query';
import { getBucketSummary, type BucketSummary } from '@/app/lib/r2cache';
import { useSyncStore } from '@/app/stores/syncStore';
import { computeBucketStatsView } from '@/app/components/status-bar-parts/bucketStatsView';

interface BucketStatsProps {
  hasConfig: boolean;
  provider?: string;
  accountId?: string;
  bucket?: string;
}

/**
 * Bucket-wide summary (total files + total size) for the status bar.
 *
 * Big-bucket friendly: while the background sync is crawling, it renders the
 * live running totals streamed by `background-sync-progress` events. Once the
 * sync is idle it asks the backend for the cached summary — a single
 * directory-tree root lookup (or one SQL aggregate before the first full
 * sync), never a full-cache scan from the UI.
 */
export default function BucketStats({ hasConfig, provider, accountId, bucket }: BucketStatsProps) {
  const backgroundSync = useSyncStore((state) => state.backgroundSync);
  const bucketSyncTimes = useSyncStore((state) => state.bucketSyncTimes);

  // Per-bucket sync time — bumps after lazy loads, full syncs and file ops,
  // so the summary refetches whenever the cache may have changed.
  const lastSyncTime = useMemo(() => {
    if (!accountId || !bucket) return null;
    return useSyncStore.getState().getLastSyncTime(accountId, bucket);
  }, [accountId, bucket, bucketSyncTimes]);

  // Key params are cache keys only — the backend resolves the active bucket
  // from app_state, like get_folder_contents and friends.
  const { data: summary } = useQuery({
    queryKey: ['bucket-summary', provider, accountId, bucket, lastSyncTime],
    queryFn: getBucketSummary,
    enabled: hasConfig && !!accountId && !!bucket && !backgroundSync.isRunning,
    staleTime: 30_000,
    retry: 1,
    // Keep showing the previous totals while a key change refetches, so the
    // stat doesn't blink out when a sync completes or the cache is bumped.
    placeholderData: (previous: BucketSummary | undefined) => previous,
  });

  const view = computeBucketStatsView({
    isSyncRunning: backgroundSync.isRunning,
    objectsFetched: backgroundSync.objectsFetched,
    bytesFetched: backgroundSync.bytesFetched,
    summary,
  });

  if (!hasConfig || !view) {
    return null;
  }

  const title = view.live
    ? 'Counting bucket…'
    : summary?.lastModified
      ? `Newest object: ${summary.lastModified}`
      : undefined;

  return (
    <span className="sb-stat bucket-stats" title={title}>
      {view.text}
    </span>
  );
}
