'use client';

import { Space } from 'antd';
import UpdateChecker from './UpdateChecker';
import {
  SyncProgress,
  FolderLoadProgress,
  BucketStats,
  DomainInfo,
  ItemsCount,
} from './status-bar-parts';

interface StatusBarProps {
  // Current view items
  totalItemsCount: number;
  searchQuery: string;
  searchTotalCount?: number;
  hasConfig: boolean;

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
      </Space>
      <DomainInfo currentConfig={currentConfig} />
    </div>
  );
}
