'use client';

import { Space } from 'antd';
import UpdateChecker from './UpdateChecker';
import {
  SyncProgress,
  FolderLoadProgress,
  BucketStats,
  DomainInfo,
  ItemsCount,
  DownloadProgress,
} from './status-bar-parts';

import type { StorageConfig } from '../lib/r2cache';

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
  return (
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
        {hasConfig && <SyncProgress />}
        <BucketStats
          hasConfig={hasConfig}
          accountId={storageConfig?.accountId}
          bucket={storageConfig?.bucket}
        />
        {!isLoadingFiles && (
          <DownloadProgress bucket={storageConfig?.bucket} accountId={storageConfig?.accountId} />
        )}
      </Space>
      <DomainInfo storageConfig={storageConfig} />
    </div>
  );
}
