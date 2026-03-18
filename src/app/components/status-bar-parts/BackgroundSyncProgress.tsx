'use client';

import { useEffect, useState } from 'react';
import { Progress } from 'antd';
import { SyncOutlined, CheckCircleOutlined } from '@ant-design/icons';
import { useSyncStore } from '@/app/stores/syncStore';

interface BackgroundSyncProgressProps {
  onClick?: () => void;
}

export default function BackgroundSyncProgress({ onClick }: BackgroundSyncProgressProps) {
  const backgroundSync = useSyncStore((state) => state.backgroundSync);
  const [showComplete, setShowComplete] = useState(false);

  // Auto-dismiss "Complete" message after 5 seconds
  useEffect(() => {
    if (backgroundSync.completedAt && !backgroundSync.isRunning) {
      setShowComplete(true);
      const timer = setTimeout(() => setShowComplete(false), 5000);
      return () => clearTimeout(timer);
    }
    setShowComplete(false);
  }, [backgroundSync.completedAt, backgroundSync.isRunning]);

  if (!backgroundSync.isRunning && !showComplete) {
    return null;
  }

  const { objectsFetched, estimatedTotal, speed } = backgroundSync;

  // Format number with locale separators
  const fmt = (n: number) => n.toLocaleString();

  if (!backgroundSync.isRunning && showComplete) {
    return (
      <span className="sync-progress" style={{ color: 'var(--ant-color-success)' }}>
        <CheckCircleOutlined />
        <span className="sync-phase">Synced {fmt(objectsFetched)} objects</span>
      </span>
    );
  }

  // Running state
  const percent =
    estimatedTotal && estimatedTotal > 0
      ? Math.min(99, Math.round((objectsFetched / estimatedTotal) * 100))
      : undefined;

  const countText = estimatedTotal
    ? `${fmt(objectsFetched)} / ~${fmt(estimatedTotal)}`
    : `${fmt(objectsFetched)}`;

  const speedText = speed > 0 ? `${Math.round(speed)}/s` : '';

  return (
    <span
      className="sync-progress"
      onClick={onClick}
      style={onClick ? { cursor: 'pointer' } : undefined}
      title="Background sync in progress — click for details"
    >
      <SyncOutlined spin style={{ fontSize: 12 }} />
      <span className="sync-phase">Syncing {countText}</span>
      {percent !== undefined && (
        <Progress
          percent={percent}
          size="small"
          showInfo={false}
          style={{ width: 60, margin: '0 4px' }}
        />
      )}
      {speedText && <span className="sync-count">{speedText}</span>}
    </span>
  );
}
