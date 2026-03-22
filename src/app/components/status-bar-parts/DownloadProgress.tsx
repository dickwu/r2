'use client';

import { useEffect, useMemo } from 'react';
import { Tooltip, Typography } from 'antd';
import {
  DownloadOutlined,
  CheckCircleOutlined,
  PauseCircleOutlined,
  ClockCircleOutlined,
} from '@ant-design/icons';
import {
  useDownloadStore,
  selectDownloadingCount,
  selectPendingCount,
  selectPausedCount,
  selectFinishedCount,
  selectHasActiveDownloads,
  selectTotalProgress,
  setupGlobalDownloadListeners,
  loadDownloadTasks,
} from '@/app/stores/downloadStore';
import { formatBytes, formatEta } from '@/app/utils/formatBytes';
import Sparkline from '@/app/components/Sparkline';

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
  const totalProgress = useDownloadStore(selectTotalProgress);
  const setModalOpen = useDownloadStore((state) => state.setModalOpen);

  // Setup global listeners once
  useEffect(() => {
    setupGlobalDownloadListeners();
  }, []);

  // Load tasks for current bucket
  useEffect(() => {
    if (!bucket || !accountId) return;
    loadDownloadTasks(bucket, accountId);
  }, [bucket, accountId]);

  // Aggregate speed across all downloading tasks
  const totalSpeed = useMemo(
    () => tasks.filter((t) => t.status === 'downloading').reduce((sum, t) => sum + t.speed, 0),
    [tasks]
  );

  // Aggregate speed history for sparkline (merge all active tasks' histories)
  const aggregateSpeedHistory = useMemo(() => {
    const activeTasks = tasks.filter(
      (t) => t.status === 'downloading' && t.speedHistory.length > 0
    );
    if (activeTasks.length === 0) return [];
    const maxLen = Math.max(...activeTasks.map((t) => t.speedHistory.length));
    const history: number[] = [];
    for (let i = 0; i < Math.min(maxLen, 30); i++) {
      let sum = 0;
      for (const t of activeTasks) {
        const offset = t.speedHistory.length - Math.min(maxLen, 30);
        const idx = offset + i;
        if (idx >= 0 && idx < t.speedHistory.length) {
          sum += t.speedHistory[idx];
        }
      }
      history.push(sum);
    }
    return history;
  }, [tasks]);

  // Calculate ETA — includes ALL pending work (downloading + queued + paused)
  const eta = useMemo(() => {
    if (totalSpeed <= 0) return '';
    const allPending = tasks.filter(
      (t) => t.status === 'downloading' || t.status === 'pending' || t.status === 'paused'
    );
    const remaining = allPending.reduce(
      (sum, t) => sum + Math.max(0, t.fileSize - t.downloadedBytes),
      0
    );
    return formatEta(remaining / totalSpeed);
  }, [tasks, totalSpeed]);

  // Per-file tooltip breakdown as React elements
  const tooltipContent = useMemo(() => {
    const active = tasks.filter(
      (t) => t.status === 'downloading' || t.status === 'pending' || t.status === 'paused'
    );
    if (active.length === 0) return null;
    return (
      <div style={{ fontSize: 11, lineHeight: '18px' }}>
        {active.slice(0, 8).map((t) => {
          const name = t.fileName.length > 25 ? t.fileName.slice(0, 22) + '...' : t.fileName;
          const info = t.speed > 0 ? `${formatBytes(t.speed)}/s` : t.status;
          return (
            <div key={t.id} style={{ display: 'flex', gap: 8, justifyContent: 'space-between' }}>
              <span style={{ opacity: 0.85 }}>{name}</span>
              <span style={{ whiteSpace: 'nowrap' }}>
                {t.progress}% · {info}
              </span>
            </div>
          );
        })}
        {active.length > 8 && (
          <div style={{ opacity: 0.5, marginTop: 2 }}>+{active.length - 8} more</div>
        )}
      </div>
    );
  }, [tasks]);

  // Don't show if no tasks (AFTER all hooks)
  if (tasks.length === 0) {
    return null;
  }

  const handleClick = () => {
    setModalOpen(true);
  };

  const activeFileCount = downloadingCount + pendingCount;

  return (
    <Tooltip title={tooltipContent} placement="top" styles={{ root: { maxWidth: 360 } }}>
      <span
        className="download-progress"
        onClick={handleClick}
        style={{
          cursor: 'pointer',
          display: 'inline-flex',
          alignItems: 'center',
          gap: 4,
          maxWidth: 360,
          overflow: 'hidden',
          flexShrink: 1,
        }}
        role="status"
        aria-live="polite"
        aria-label={`Download progress: ${totalProgress}%`}
      >
        {hasActiveDownloads ? (
          <>
            {/* Mini aggregate progress bar */}
            <span
              style={{
                display: 'inline-block',
                width: 48,
                height: 4,
                borderRadius: 2,
                backgroundColor: 'var(--color-border-control)',
                overflow: 'hidden',
                verticalAlign: 'middle',
              }}
              role="progressbar"
              aria-valuenow={totalProgress}
              aria-valuemin={0}
              aria-valuemax={100}
            >
              <span
                style={{
                  display: 'block',
                  width: `${totalProgress}%`,
                  height: '100%',
                  borderRadius: 2,
                  backgroundColor: 'var(--color-link)',
                  transition: 'width 0.3s ease',
                }}
              />
            </span>
            <DownloadOutlined style={{ fontSize: 12 }} />
            <Text style={{ fontSize: 12 }}>
              {activeFileCount} file{activeFileCount !== 1 ? 's' : ''}
            </Text>
            {totalSpeed > 0 && (
              <Text type="secondary" style={{ fontSize: 12 }}>
                {formatBytes(totalSpeed)}/s
              </Text>
            )}
            {eta && (
              <Text type="secondary" style={{ fontSize: 12 }}>
                {eta}
              </Text>
            )}
            <Sparkline
              data={aggregateSpeedHistory}
              style={{ marginLeft: 6, verticalAlign: 'middle', opacity: 0.7 }}
            />
          </>
        ) : pendingCount > 0 ? (
          <>
            <ClockCircleOutlined style={{ color: 'var(--color-link)' }} />
            <Text style={{ fontSize: 12 }}>{pendingCount} queued</Text>
          </>
        ) : pausedCount > 0 ? (
          <>
            <PauseCircleOutlined style={{ color: 'var(--color-warning, #faad14)' }} />
            <Text style={{ fontSize: 12 }}>{pausedCount} paused</Text>
          </>
        ) : (
          <>
            <CheckCircleOutlined style={{ color: 'var(--color-success)' }} />
            <Text style={{ fontSize: 12 }}>{finishedCount} downloaded</Text>
          </>
        )}
      </span>
    </Tooltip>
  );
}
