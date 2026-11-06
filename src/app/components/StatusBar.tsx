'use client';

import { useTranslation } from 'react-i18next';
import { useMemo, useState } from 'react';
import { Modal } from 'antd';
import { useAntdModalFocusable } from '@/app/hooks/useAntdModalFocusable';
import { DatabaseOutlined, SwapOutlined } from '@ant-design/icons';
import { useSyncStore } from '@/app/stores/syncStore';
import { useUploadStore } from '@/app/stores/uploadStore';
import { useDownloadStore } from '@/app/stores/downloadStore';
import { useMoveStore, selectAttentionCount } from '@/app/stores/moveStore';
import SyncOverlay from '@/app/components/SyncOverlay';
import UpdateChecker from '@/app/components/UpdateChecker';
import ReportProblemButton from '@/app/components/report/ReportProblemButton';
import BucketStats from '@/app/components/status-bar-parts/BucketStats';
import type { StorageConfig } from '@/app/lib/r2cache';
import { countActiveTransfers } from '@/app/lib/updates/activeWork';

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
  const { t } = useTranslation();
  if (!timestamp) return '';
  const diffMs = Date.now() - timestamp;
  const diffMin = Math.floor(diffMs / 60_000);
  if (diffMin < 1) return t('time.justNow');
  if (diffMin < 60) return t('time.minAgo', { count: diffMin });
  const diffHr = Math.floor(diffMin / 60);
  return t('time.hrAgo', { count: diffHr });
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
  const { t } = useTranslation();
  const [syncDetailsOpen, setSyncDetailsOpen] = useState(false);
  const focusable = useAntdModalFocusable();

  // Sync state
  const isSyncing = useSyncStore((s) => s.isSyncing);
  const phase = useSyncStore((s) => s.phase);
  const currentBucketKey = useSyncStore((s) => s.currentBucketKey);
  const bucketSyncTimes = useSyncStore((s) => s.bucketSyncTimes);

  const lastSyncTime = useMemo(() => {
    if (!storageConfig?.accountId || !storageConfig?.bucket) return null;
    const key = `${storageConfig.provider}:${storageConfig.accountId}:${storageConfig.bucket}`;
    return bucketSyncTimes[key] ?? null;
  }, [
    storageConfig?.provider,
    storageConfig?.accountId,
    storageConfig?.bucket,
    bucketSyncTimes,
    currentBucketKey,
  ]);

  const relativeTime = useRelativeTime(lastSyncTime);

  // Transfer counts from stores
  const uploadTasks = useUploadStore((s) => s.tasks);
  const downloadTasks = useDownloadStore((s) => s.tasks);
  const moveTasks = useMoveStore((s) => s.tasks);
  const moveAttentionCount = useMoveStore(selectAttentionCount);

  const transferCount = useMemo(
    () => countActiveTransfers(uploadTasks, downloadTasks, moveTasks),
    [uploadTasks, downloadTasks, moveTasks]
  );

  // Sync state class
  const syncClass =
    isSyncing || (phase !== 'idle' && phase !== 'complete')
      ? 'syncing'
      : lastSyncTime
        ? 'synced'
        : '';

  const syncLabel =
    syncClass === 'syncing'
      ? t('status.syncingShort')
      : syncClass === 'synced'
        ? t('status.synced', { time: relativeTime })
        : t('status.idle');

  // Item count label
  const itemLabel = useMemo(() => {
    if (!hasConfig) return '';
    if (searchQuery.trim()) {
      return t('status.results', {
        count: searchTotalCount,
        formatted: searchTotalCount.toLocaleString(),
      });
    }
    return t('status.items', {
      count: totalItemsCount,
      formatted: totalItemsCount.toLocaleString(),
    });
  }, [hasConfig, searchQuery, searchTotalCount, totalItemsCount, t]);

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
            {selectedCount > 0 && <span> · {t('status.selected', { count: selectedCount })}</span>}
          </span>
        )}

        {/* Bucket name */}
        {storageConfig?.bucket && (
          <span className="sb-stat">
            <DatabaseOutlined style={{ fontSize: 11 }} />
            {storageConfig.bucket}
          </span>
        )}

        {/* Bucket-wide summary (live while a big bucket is syncing) */}
        <BucketStats
          hasConfig={hasConfig}
          provider={storageConfig?.provider}
          accountId={storageConfig?.accountId}
          bucket={storageConfig?.bucket}
        />

        <span className="spacer" />

        {moveAttentionCount > 0 && (
          <button className="sb-stat" onClick={() => useMoveStore.getState().setModalOpen(true)}>
            {t('status.movesNeedAttention', { count: moveAttentionCount })}
          </button>
        )}

        {/* Active transfers */}
        {transferCount > 0 && !isLoadingFiles && (
          <span className="sb-stat" style={{ color: 'var(--accent)' }}>
            <SwapOutlined style={{ fontSize: 11 }} />
            {t('status.transfersActive', { count: transferCount })}
          </span>
        )}

        {/* Report a problem (opens a prefilled GitHub issue) */}
        <ReportProblemButton />

        {/* App version, which becomes the update's state (and hosts the update dialog) */}
        <UpdateChecker />
      </div>

      <Modal
        title={t('status.syncProgress')}
        open={syncDetailsOpen}
        onCancel={() => setSyncDetailsOpen(false)}
        footer={null}
        width={520}
        destroyOnHidden={false}
        focusable={focusable}
      >
        <SyncOverlay />
      </Modal>
    </>
  );
}
