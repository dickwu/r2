'use client';

import { useMemo, useState } from 'react';
import { Modal } from 'antd';
import { DatabaseOutlined, SwapOutlined } from '@ant-design/icons';
import { useSyncStore } from '@/app/stores/syncStore';
import { useUploadStore } from '@/app/stores/uploadStore';
import { useDownloadStore } from '@/app/stores/downloadStore';
import { useMoveStore } from '@/app/stores/moveStore';
import SyncOverlay from '@/app/components/SyncOverlay';
import UpdateChecker from '@/app/components/UpdateChecker';
import type { StorageConfig } from '@/app/lib/r2cache';

interface StatusBarProps {
  totalItemsCount: number;
  searchQuery: string;
  searchTotalCount?: number;
  hasConfig: boolean;
  isLoadingFiles?: boolean;
  storageConfig: StorageConfig | null;
  selectedCount?: number;
}

function useRelativeTime(timestamp: number | null): string {
  if (!timestamp) return '';
  const diffMs = Date.now() - timestamp;
  const diffMin = Math.floor(diffMs / 60_000);
  if (diffMin < 1) return 'just now';
  if (diffMin === 1) return '1 min ago';
  if (diffMin < 60) return `${diffMin} min ago`;
  const diffHr = Math.floor(diffMin / 60);
  if (diffHr === 1) return '1 hr ago';
  return `${diffHr} hr ago`;
}

export default function StatusBar({
  totalItemsCount,
  searchQuery,
  searchTotalCount = 0,
  hasConfig,
  isLoadingFiles = false,
  storageConfig,
  selectedCount = 0,
}: StatusBarProps) {
  const [syncDetailsOpen, setSyncDetailsOpen] = useState(false);

  // Sync state
  const isSyncing = useSyncStore((s) => s.isSyncing);
  const phase = useSyncStore((s) => s.phase);
  const currentBucketKey = useSyncStore((s) => s.currentBucketKey);
  const bucketSyncTimes = useSyncStore((s) => s.bucketSyncTimes);

  const lastSyncTime = useMemo(() => {
    if (!storageConfig?.accountId || !storageConfig?.bucket) return null;
    const key = `${storageConfig.accountId}:${storageConfig.bucket}`;
    return bucketSyncTimes[key] ?? null;
  }, [storageConfig?.accountId, storageConfig?.bucket, bucketSyncTimes, currentBucketKey]);

  const relativeTime = useRelativeTime(lastSyncTime);

  // Transfer counts from stores
  const uploadTasks = useUploadStore((s) => s.tasks);
  const downloadTasks = useDownloadStore((s) => s.tasks);
  const moveTasks = useMoveStore((s) => s.tasks);

  const transferCount = useMemo(() => {
    const activeUploads = uploadTasks.filter(
      (t) => t.status === 'uploading' || t.status === 'pending'
    ).length;
    const activeDownloads = downloadTasks.filter(
      (t) => t.status === 'downloading' || t.status === 'pending'
    ).length;
    const activeMoves = moveTasks.filter(
      (t) =>
        t.status === 'downloading' ||
        t.status === 'uploading' ||
        t.status === 'finishing' ||
        t.status === 'deleting' ||
        t.status === 'pending'
    ).length;
    return activeUploads + activeDownloads + activeMoves;
  }, [uploadTasks, downloadTasks, moveTasks]);

  // Sync state class
  const syncClass =
    isSyncing || (phase !== 'idle' && phase !== 'complete')
      ? 'syncing'
      : lastSyncTime
        ? 'synced'
        : '';

  const syncLabel =
    syncClass === 'syncing'
      ? 'Syncing…'
      : syncClass === 'synced'
        ? `Synced ${relativeTime}`
        : 'Idle';

  // Item count label
  const itemLabel = useMemo(() => {
    if (!hasConfig) return '';
    if (searchQuery.trim()) {
      return `${searchTotalCount.toLocaleString()} results`;
    }
    return `${totalItemsCount.toLocaleString()} items`;
  }, [hasConfig, searchQuery, searchTotalCount, totalItemsCount]);

  return (
    <>
      <div className="statusbar">
        {/* Sync state */}
        {hasConfig && (
          <span
            className={`sb-stat${syncClass ? ` ${syncClass}` : ''}`}
            style={{ cursor: syncClass === 'syncing' ? 'pointer' : undefined }}
            onClick={syncClass === 'syncing' ? () => setSyncDetailsOpen(true) : undefined}
          >
            <span className="dot" />
            {syncLabel}
          </span>
        )}

        {/* Item count + selection */}
        {itemLabel && (
          <span className="sb-stat">
            {itemLabel}
            {selectedCount > 0 && <span> · {selectedCount} selected</span>}
          </span>
        )}

        {/* Bucket name */}
        {storageConfig?.bucket && (
          <span className="sb-stat">
            <DatabaseOutlined style={{ fontSize: 11 }} />
            {storageConfig.bucket}
          </span>
        )}

        <span className="spacer" />

        {/* Active transfers */}
        {transferCount > 0 && !isLoadingFiles && (
          <span className="sb-stat" style={{ color: 'var(--accent)' }}>
            <SwapOutlined style={{ fontSize: 11 }} />
            {transferCount} transfer{transferCount > 1 ? 's' : ''} active
          </span>
        )}

        {/* Update checker (shows app version + update-available badge) */}
        <UpdateChecker />
      </div>

      <Modal
        title="Sync Progress"
        open={syncDetailsOpen}
        onCancel={() => setSyncDetailsOpen(false)}
        footer={null}
        width={520}
        destroyOnHidden={false}
      >
        <SyncOverlay />
      </Modal>
    </>
  );
}
