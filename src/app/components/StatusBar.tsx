'use client';

import { useState } from 'react';
import { Modal, Space } from 'antd';
import UpdateChecker from '@/app/components/UpdateChecker';
import SyncOverlay from '@/app/components/SyncOverlay';
import {
  SyncProgress,
  FolderLoadProgress,
  BucketStats,
  DomainInfo,
  ItemsCount,
  DownloadProgress,
  MoveProgress,
} from '@/app/components/status-bar-parts';

import type { StorageConfig } from '@/app/lib/r2cache';

interface StatusBarProps {
  // Current view items
  totalItemsCount: number;
  searchQuery: string;
  searchTotalCount?: number;
  hasConfig: boolean;
  isLoadingFiles?: boolean;

  // Bucket info
  storageConfig: StorageConfig | null;
}

export default function StatusBar({
  totalItemsCount,
  searchQuery,
  searchTotalCount = 0,
  hasConfig,
  isLoadingFiles = false,
  storageConfig,
}: StatusBarProps) {
  const [syncDetailsOpen, setSyncDetailsOpen] = useState(false);

  return (
    <>
      <div className="status-bar">
        <Space size="middle">
          <UpdateChecker />
          <ItemsCount
            hasConfig={hasConfig}
            searchQuery={searchQuery}
            searchTotalCount={searchTotalCount}
            totalItemsCount={totalItemsCount}
          />
          {hasConfig && <FolderLoadProgress />}
          {hasConfig && <SyncProgress onClick={() => setSyncDetailsOpen(true)} />}
          <BucketStats
            hasConfig={hasConfig}
            accountId={storageConfig?.accountId}
            bucket={storageConfig?.bucket}
          />
          <MoveProgress
            sourceBucket={storageConfig?.bucket}
            sourceAccountId={storageConfig?.accountId}
          />
          {!isLoadingFiles && (
            <DownloadProgress bucket={storageConfig?.bucket} accountId={storageConfig?.accountId} />
          )}
        </Space>
        <DomainInfo storageConfig={storageConfig} />
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
