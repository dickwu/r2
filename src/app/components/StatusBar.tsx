'use client';

import { useEffect, useMemo } from 'react';
import { Space, Spin } from 'antd';
import {
  CloudDownloadOutlined,
  DatabaseOutlined,
  BuildOutlined,
  CheckCircleOutlined,
} from '@ant-design/icons';
import UpdateChecker from './UpdateChecker';
import { useFolderSizeStore } from '../stores/folderSizeStore';
import { useSyncStore, SyncPhase } from '../stores/syncStore';
import { formatBytes } from '../utils/formatBytes';

interface StatusBarProps {
  // Current view items
  filteredItemsCount: number;
  totalItemsCount: number;
  searchQuery: string;
  searchTotalCount?: number; // Total matching files in bucket (from DB search)
  hasConfig: boolean;

  // Bucket info
  currentConfig: {
    account_id: string;
    public_domain?: string | null;
    access_key_id?: string | null;
  } | null;

  // Sync state
  isSyncing?: boolean;
  lastSyncTime?: number | null;
}

// Phase display configuration
const phaseConfig: Record<SyncPhase, { icon: React.ReactNode; label: string }> = {
  idle: { icon: null, label: '' },
  fetching: { icon: <CloudDownloadOutlined />, label: 'Fetching' },
  storing: { icon: <DatabaseOutlined />, label: 'Storing' },
  indexing: { icon: <BuildOutlined />, label: 'Indexing' },
  complete: { icon: <CheckCircleOutlined />, label: 'Complete' },
};

export default function StatusBar({
  filteredItemsCount,
  totalItemsCount,
  searchQuery,
  searchTotalCount = 0,
  hasConfig,
  currentConfig,
  isSyncing = false,
  lastSyncTime,
}: StatusBarProps) {
  const metadata = useFolderSizeStore((state) => state.metadata);
  const loadMetadata = useFolderSizeStore((state) => state.loadMetadata);
  const setMetadata = useFolderSizeStore((state) => state.setMetadata);
  const phase = useSyncStore((state) => state.phase);
  const processedFiles = useSyncStore((state) => state.processedFiles);
  const totalFiles = useSyncStore((state) => state.totalFiles);

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

  // Render sync progress with phase indicator
  const renderSyncProgress = () => {
    if (!isSyncing || phase === 'idle' || phase === 'complete') {
      return null;
    }

    const { icon, label } = phaseConfig[phase];
    const fileCount = totalFiles > 0 ? totalFiles : processedFiles;

    return (
      <span className="sync-progress">
        <Spin size="small" />
        <span className="sync-phase">
          {icon} {label}
        </span>
        {phase === 'fetching' && (
          <span className="sync-count">{processedFiles.toLocaleString()} files</span>
        )}
        {(phase === 'storing' || phase === 'indexing') && fileCount > 0 && (
          <span className="sync-count">{fileCount.toLocaleString()} files</span>
        )}
      </span>
    );
  };

  return (
    <div className="status-bar">
      <Space size="middle">
        <UpdateChecker />

        {/* Current view items count */}
        {hasConfig && (
          <span>
            {searchQuery
              ? `${searchTotalCount.toLocaleString()} result${searchTotalCount !== 1 ? 's' : ''}`
              : `${totalItemsCount} items`}
          </span>
        )}

        {/* Sync progress with phase */}
        {hasConfig && renderSyncProgress()}

        {/* Bucket-wide statistics (show when not syncing) */}
        {hasConfig && !isSyncing && !bucketStats.loading && bucketStats.totalFiles !== null && (
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
