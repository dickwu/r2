import React from 'react';
import { useTranslation } from 'react-i18next';
import { Spin } from 'antd';
import {
  CloudDownloadOutlined,
  DatabaseOutlined,
  BuildOutlined,
  CheckCircleOutlined,
} from '@ant-design/icons';
import { useSyncStore, SyncPhase } from '@/app/stores/syncStore';

// Sync progress overlay for first-load experience
export default function SyncOverlay() {
  const phase = useSyncStore((state) => state.phase);
  const processedFiles = useSyncStore((state) => state.processedFiles);
  const storedFiles = useSyncStore((state) => state.storedFiles);
  const totalFiles = useSyncStore((state) => state.totalFiles);
  const indexingProgress = useSyncStore((state) => state.indexingProgress);
  const { t } = useTranslation();

  const phaseInfo: Record<
    SyncPhase,
    { icon: React.ReactNode; title: string; description: string }
  > = {
    idle: {
      icon: <CloudDownloadOutlined />,
      title: t('sync.overlay.idle.title'),
      description: t('sync.overlay.idle.description'),
    },
    fetching: {
      icon: <CloudDownloadOutlined />,
      title: t('sync.overlay.fetching.title'),
      description: t('sync.overlay.fetching.description'),
    },
    storing: {
      icon: <DatabaseOutlined />,
      title: t('sync.overlay.storing.title'),
      description: t('sync.overlay.storing.description'),
    },
    indexing: {
      icon: <BuildOutlined />,
      title: t('sync.overlay.indexing.title'),
      description: t('sync.overlay.indexing.description'),
    },
    complete: {
      icon: <CheckCircleOutlined />,
      title: t('sync.overlay.complete.title'),
      description: t('sync.overlay.complete.description'),
    },
  };

  const { icon, title, description } = phaseInfo[phase];

  // Calculate progress display
  let progressDisplay = null;
  if (phase === 'fetching' && processedFiles > 0) {
    progressDisplay = (
      <div className="sync-overlay-progress">
        <span className="sync-overlay-count">{processedFiles.toLocaleString()}</span>
        <span className="sync-overlay-label">{t('sync.overlay.filesFound')}</span>
      </div>
    );
  } else if (phase === 'storing') {
    const total = processedFiles || totalFiles;
    progressDisplay = (
      <div className="sync-overlay-progress">
        <span className="sync-overlay-count">
          {storedFiles.toLocaleString()}
          {total > 0 ? ` / ${total.toLocaleString()}` : ''}
        </span>
        <span className="sync-overlay-label">{t('sync.overlay.filesCached')}</span>
      </div>
    );
  } else if (phase === 'indexing') {
    const { current, total } = indexingProgress;
    if (total > 0) {
      const percentage = Math.round((current / total) * 100);
      progressDisplay = (
        <div className="sync-overlay-progress">
          <div className="sync-overlay-bar">
            <div className="sync-overlay-bar-fill" style={{ width: `${percentage}%` }} />
          </div>
          <span className="sync-overlay-percentage">{percentage}%</span>
        </div>
      );
    }
  }

  return (
    <div className="sync-overlay">
      <div className="sync-overlay-content">
        <div className="sync-overlay-icon">{icon}</div>
        <h3 className="sync-overlay-title">{title}</h3>
        <p className="sync-overlay-description">{description}</p>
        {progressDisplay}
        {phase !== 'complete' && <Spin size="small" style={{ marginTop: 16 }} />}
      </div>
    </div>
  );
}
