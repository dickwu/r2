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

interface StatusBarProps {
  // Current view items
  totalItemsCount: number;
  searchQuery: string;
  searchTotalCount?: number;
  hasConfig: boolean;
  isLoadingFiles?: boolean;

  // Bucket info
  currentConfig: {
    account_id: string;
    bucket: string;
    public_domain?: string | null;
    access_key_id?: string | null;
  } | null;
}

export default function StatusBar({
  totalItemsCount,
  searchQuery,
  searchTotalCount = 0,
  hasConfig,
  isLoadingFiles = false,
  currentConfig,
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
          accountId={currentConfig?.account_id}
          bucket={currentConfig?.bucket}
        />
        {!isLoadingFiles && (
          <DownloadProgress bucket={currentConfig?.bucket} accountId={currentConfig?.account_id} />
        )}
      </Space>
      <DomainInfo currentConfig={currentConfig} />
    </div>
  );
}
