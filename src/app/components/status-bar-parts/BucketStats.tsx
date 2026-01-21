'use client';

import { useEffect, useMemo } from 'react';
import { useFolderSizeStore } from '@/app/stores/folderSizeStore';
import { useSyncStore } from '@/app/stores/syncStore';
import { formatBytes } from '@/app/utils/formatBytes';

interface BucketStatsProps {
  hasConfig: boolean;
  accountId?: string;
  bucket?: string;
}

export default function BucketStats({ hasConfig, accountId, bucket }: BucketStatsProps) {
  const metadata = useFolderSizeStore((state) => state.metadata);
  const loadMetadata = useFolderSizeStore((state) => state.loadMetadata);
  const setMetadata = useFolderSizeStore((state) => state.setMetadata);
  const isSyncing = useSyncStore((state) => state.isSyncing);
  const bucketSyncTimes = useSyncStore((state) => state.bucketSyncTimes);

  // Get per-bucket sync time
  const lastSyncTime = useMemo(() => {
    if (!accountId || !bucket) return null;
    return useSyncStore.getState().getLastSyncTime(accountId, bucket);
  }, [accountId, bucket, bucketSyncTimes]);

  // Clear root metadata when sync completes to force fresh load
  useEffect(() => {
    if (lastSyncTime) {
      setMetadata('', {
        size: 'loading',
        fileCount: null,
        totalFileCount: null,
        lastModified: null,
      });
    }
  }, [lastSyncTime, setMetadata]);

  // Load root directory metadata (empty string = root)
  useEffect(() => {
    if (hasConfig && !isSyncing) {
      loadMetadata('');
    }
  }, [hasConfig, isSyncing, lastSyncTime, loadMetadata]);

  // Get bucket-wide statistics from root metadata
  const bucketStats = useMemo(() => {
    const rootMetadata = metadata[''];
    if (!rootMetadata) {
      return { totalFiles: null, totalSize: null, loading: true };
    }

    return {
      totalFiles: rootMetadata.totalFileCount,
      totalSize: typeof rootMetadata.size === 'number' ? rootMetadata.size : null,
      loading: rootMetadata.size === 'loading',
    };
  }, [metadata]);

  if (!hasConfig || isSyncing || bucketStats.loading || bucketStats.totalFiles === null) {
    return null;
  }

  return (
    <span className="bucket-stats">
      Bucket: {bucketStats.totalFiles.toLocaleString()} files
      {bucketStats.totalSize !== null && ` · ${formatBytes(bucketStats.totalSize)}`}
    </span>
  );
}
