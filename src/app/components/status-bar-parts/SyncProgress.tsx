'use client';

import { useTranslation } from 'react-i18next';
import { Spin } from 'antd';
import {
  CloudDownloadOutlined,
  DatabaseOutlined,
  BuildOutlined,
  CheckCircleOutlined,
} from '@ant-design/icons';
import { useSyncStore, SyncPhase } from '@/app/stores/syncStore';

const phaseConfig: Record<SyncPhase, { icon: React.ReactNode; label: string }> = {
  idle: { icon: null, label: '' },
  fetching: { icon: <CloudDownloadOutlined />, label: 'status.phase.fetching' },
  storing: { icon: <DatabaseOutlined />, label: 'status.phase.storing' },
  indexing: { icon: <BuildOutlined />, label: 'status.phase.indexing' },
  complete: { icon: <CheckCircleOutlined />, label: 'status.complete' },
};

interface SyncProgressProps {
  onClick?: () => void;
}

export default function SyncProgress({ onClick }: SyncProgressProps) {
  const isSyncing = useSyncStore((state) => state.isSyncing);
  const phase = useSyncStore((state) => state.phase);
  const processedFiles = useSyncStore((state) => state.processedFiles);
  const storedFiles = useSyncStore((state) => state.storedFiles);
  const totalFiles = useSyncStore((state) => state.totalFiles);
  const indexingProgress = useSyncStore((state) => state.indexingProgress);
  const { t } = useTranslation();

  if (!isSyncing || phase === 'idle' || phase === 'complete') {
    return null;
  }

  const { icon, label } = phaseConfig[phase];

  // Calculate progress text based on phase
  let progressText = '';
  if (phase === 'fetching') {
    progressText = t('status.files', {
      count: processedFiles,
      formatted: processedFiles.toLocaleString(),
    });
  } else if (phase === 'storing') {
    const total = processedFiles || totalFiles;
    progressText =
      total > 0
        ? t('status.filesOf', {
            count: total,
            done: storedFiles.toLocaleString(),
            total: total.toLocaleString(),
          })
        : t('status.files', { count: storedFiles, formatted: storedFiles.toLocaleString() });
  } else if (phase === 'indexing') {
    const { current, total } = indexingProgress;
    if (total > 0) {
      const percentage = Math.round((current / total) * 100);
      progressText = `${percentage}% (${current.toLocaleString()}/${total.toLocaleString()})`;
    } else if (totalFiles > 0) {
      progressText = t('status.files', {
        count: totalFiles,
        formatted: totalFiles.toLocaleString(),
      });
    }
  }

  return (
    <span
      className="sync-progress"
      onClick={onClick}
      style={onClick ? { cursor: 'pointer' } : undefined}
      title={onClick ? t('status.syncDetailsHint') : undefined}
    >
      <Spin size="small" />
      <span className="sync-phase">
        {icon} {t(label)}
      </span>
      {progressText && <span className="sync-count">{progressText}</span>}
    </span>
  );
}
