'use client';

import { useEffect } from 'react';
import { Spin, Typography } from 'antd';
import {
  DownloadOutlined,
  CheckCircleOutlined,
  PauseCircleOutlined,
  ClockCircleOutlined,
} from '@ant-design/icons';
import { invoke } from '@tauri-apps/api/core';
import {
  useDownloadStore,
  selectDownloadingCount,
  selectPendingCount,
  selectPausedCount,
  selectFinishedCount,
  selectHasActiveDownloads,
  MAX_CONCURRENT_DOWNLOADS,
  DownloadSession,
  setupDownloadEventListeners,
} from '../../stores/downloadStore';
import { formatBytes } from '../../utils/formatBytes';

const { Text } = Typography;

interface DownloadProgressProps {
  bucket?: string;
  accountId?: string;
}

export default function DownloadProgress({ bucket, accountId }: DownloadProgressProps) {
  const tasks = useDownloadStore((state) => state.tasks);
  const downloadingCount = useDownloadStore(selectDownloadingCount);
  const pendingCount = useDownloadStore(selectPendingCount);
  const pausedCount = useDownloadStore(selectPausedCount);
  const finishedCount = useDownloadStore(selectFinishedCount);
  const hasActiveDownloads = useDownloadStore(selectHasActiveDownloads);
  const setModalOpen = useDownloadStore((state) => state.setModalOpen);
  const loadFromDatabase = useDownloadStore((state) => state.loadFromDatabase);

  // Load tasks from database and setup event listeners on mount
  useEffect(() => {
    if (!bucket || !accountId) return;

    let cleanup: (() => void) | undefined;

    const initialize = async () => {
      try {
        // Load initial tasks from database
        const sessions = await invoke<DownloadSession[]>('get_download_tasks', {
          bucket,
          accountId,
        });
        loadFromDatabase(sessions);

        // Setup event listeners for real-time updates
        cleanup = await setupDownloadEventListeners(bucket, accountId);
      } catch (e) {
        console.error('Failed to initialize download tasks:', e);
      }
    };

    initialize();

    return () => {
      if (cleanup) cleanup();
    };
  }, [bucket, accountId, loadFromDatabase]);

  // Don't show if no tasks
  if (tasks.length === 0) {
    return null;
  }

  // Calculate total speed
  const totalSpeed = tasks
    .filter((t) => t.status === 'downloading')
    .reduce((sum, t) => sum + t.speed, 0);

  const handleClick = () => {
    setModalOpen(true);
  };

  return (
    <span
      className="download-progress"
      onClick={handleClick}
      style={{ cursor: 'pointer' }}
      title="Click to view download details"
    >
      {hasActiveDownloads ? (
        <>
          <Spin size="small" />
          <DownloadOutlined style={{ marginLeft: 4 }} />
          <Text style={{ marginLeft: 4 }}>
            {downloadingCount}/{MAX_CONCURRENT_DOWNLOADS} downloading
            {pendingCount > 0 && ` (${pendingCount} queued)`}
          </Text>
          {totalSpeed > 0 && (
            <Text type="secondary" style={{ marginLeft: 4 }}>
              {formatBytes(totalSpeed)}/s
            </Text>
          )}
        </>
      ) : pendingCount > 0 ? (
        <>
          <ClockCircleOutlined style={{ color: '#1677ff' }} />
          <Text style={{ marginLeft: 4 }}>{pendingCount} queued</Text>
        </>
      ) : pausedCount > 0 ? (
        <>
          <PauseCircleOutlined style={{ color: '#faad14' }} />
          <Text style={{ marginLeft: 4 }}>{pausedCount} paused</Text>
        </>
      ) : (
        <>
          <CheckCircleOutlined style={{ color: '#52c41a' }} />
          <Text style={{ marginLeft: 4 }}>{finishedCount} downloaded</Text>
        </>
      )}
    </span>
  );
}
