'use client';

import { useTranslation } from 'react-i18next';
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
  const { t } = useTranslation();
  const completeToken = backgroundSync.isRunning
    ? 'running'
    : backgroundSync.completedAt
      ? `done:${backgroundSync.completedAt}`
      : 'idle';
  const [seenCompleteToken, setSeenCompleteToken] = useState(completeToken);
  if (completeToken !== seenCompleteToken) {
    setSeenCompleteToken(completeToken);
    setShowComplete(completeToken.startsWith('done:'));
  }

  // Auto-dismiss "Complete" message after 5 seconds
  useEffect(() => {
    if (!showComplete) return;
    const timer = setTimeout(() => setShowComplete(false), 5000);
    return () => clearTimeout(timer);
  }, [showComplete, completeToken]);

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
        <span className="sync-phase">
          {t('status.syncedObjects', { count: objectsFetched, formatted: fmt(objectsFetched) })}
        </span>
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
    <button
      type="button"
      className="sync-progress"
      onClick={onClick}
      style={onClick ? { cursor: 'pointer' } : undefined}
      title={t('status.backgroundSyncHint')}
    >
      <SyncOutlined spin style={{ fontSize: 12 }} />
      <span className="sync-phase">{t('status.syncing', { progress: countText })}</span>
      {percent !== undefined && (
        <Progress
          percent={percent}
          size="small"
          showInfo={false}
          style={{ width: 60, margin: '0 4px' }}
        />
      )}
      {speedText && <span className="sync-count">{speedText}</span>}
    </button>
  );
}
