'use client';

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
  fetching: { icon: <CloudDownloadOutlined />, label: 'Fetching' },
  storing: { icon: <DatabaseOutlined />, label: 'Storing' },
  indexing: { icon: <BuildOutlined />, label: 'Indexing' },
  complete: { icon: <CheckCircleOutlined />, label: 'Complete' },
};

interface SyncProgressProps {
  onClick?: () => void;
}

export default function SyncProgress({ onClick }: SyncProgressProps) {
  const isSyncing = useSyncStore((state) => state.isSyncing);
  const phase = useSyncStore((state) => state.phase);
  const processedFiles = useSyncStore((state) => state.processedFiles);
  const totalFiles = useSyncStore((state) => state.totalFiles);
  const indexingProgress = useSyncStore((state) => state.indexingProgress);

  if (!isSyncing || phase === 'idle' || phase === 'complete') {
    return null;
  }

  const { icon, label } = phaseConfig[phase];

  // Calculate progress text based on phase
  let progressText = '';
  if (phase === 'fetching') {
    progressText = `${processedFiles.toLocaleString()} files`;
  } else if (phase === 'storing') {
    progressText = `${totalFiles.toLocaleString()} files`;
  } else if (phase === 'indexing') {
    const { current, total } = indexingProgress;
    if (total > 0) {
      const percentage = Math.round((current / total) * 100);
      progressText = `${percentage}% (${current.toLocaleString()}/${total.toLocaleString()})`;
    } else if (totalFiles > 0) {
      progressText = `${totalFiles.toLocaleString()} files`;
    }
  }

  return (
    <span
      className="sync-progress"
      onClick={onClick}
      style={onClick ? { cursor: 'pointer' } : undefined}
      title={onClick ? 'Click to view sync details' : undefined}
    >
      <Spin size="small" />
      <span className="sync-phase">
        {icon} {label}
      </span>
      {progressText && <span className="sync-count">{progressText}</span>}
    </span>
  );
}
