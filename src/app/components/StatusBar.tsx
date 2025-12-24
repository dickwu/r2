'use client';

import { useEffect, useMemo } from 'react';
import { Space, Spin } from 'antd';
import UpdateChecker from './UpdateChecker';
import { useFolderSizeStore } from '../stores/folderSizeStore';
import { formatBytes } from '../utils/formatBytes';

interface StatusBarProps {
  // Current view items
  filteredItemsCount: number;
  totalItemsCount: number;
  searchQuery: string;
  hasConfig: boolean;

  // Bucket info
  currentConfig: {
    account_id: string;
    public_domain?: string | null;
    access_key_id?: string | null;
  } | null;

  // Sync state
  isSyncing?: boolean;
}

export default function StatusBar({
  filteredItemsCount,
  totalItemsCount,
  searchQuery,
  hasConfig,
  currentConfig,
  isSyncing = false,
}: StatusBarProps) {
  const metadata = useFolderSizeStore((state) => state.metadata);
  const loadMetadata = useFolderSizeStore((state) => state.loadMetadata);

  // Load root directory metadata (empty string = root)
  useEffect(() => {
    if (hasConfig && !isSyncing) {
      loadMetadata('');
    }
  }, [hasConfig, isSyncing, loadMetadata]);

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

  return (
    <div className="status-bar">
      <Space size="middle">
        <UpdateChecker />

        {/* Current view items count */}
        {hasConfig && (
          <span>
            {searchQuery
              ? `${filteredItemsCount} of ${totalItemsCount} items`
              : `${totalItemsCount} items`}
          </span>
        )}

        {/* Bucket-wide statistics */}
        {hasConfig && bucketStats.loading && (
          <span>
            <Spin size="small" /> Loading bucket info...
          </span>
        )}

        {hasConfig && !bucketStats.loading && bucketStats.totalFiles !== null && (
          <span className="bucket-stats">
            Bucket: {bucketStats.totalFiles.toLocaleString()} files
            {bucketStats.totalSize !== null && ` · ${formatBytes(bucketStats.totalSize)}`}
          </span>
        )}
      </Space>

      {/* Domain info */}
      {currentConfig && (
        <span className="domain">
          {currentConfig.public_domain
            ? currentConfig.public_domain
            : currentConfig.access_key_id
              ? `${currentConfig.account_id}.r2.cloudflarestorage.com (signed)`
              : `${currentConfig.account_id}.r2.cloudflarestorage.com`}
        </span>
      )}
    </div>
  );
}
