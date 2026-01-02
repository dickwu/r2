import React from 'react';
import { Spin } from 'antd';
import {
  CloudDownloadOutlined,
  DatabaseOutlined,
  BuildOutlined,
  CheckCircleOutlined,
} from '@ant-design/icons';
import { useSyncStore, SyncPhase } from '../stores/syncStore';

// Sync progress overlay for first-load experience
export default function SyncOverlay() {
  const phase = useSyncStore((state) => state.phase);
  const processedFiles = useSyncStore((state) => state.processedFiles);
  const totalFiles = useSyncStore((state) => state.totalFiles);
  const indexingProgress = useSyncStore((state) => state.indexingProgress);

  const phaseInfo: Record<
    SyncPhase,
    { icon: React.ReactNode; title: string; description: string }
  > = {
    idle: {
      icon: <CloudDownloadOutlined />,
      title: 'Preparing...',
      description: 'Getting ready to sync your bucket',
    },
    fetching: {
      icon: <CloudDownloadOutlined />,
      title: 'Fetching Files',
      description: 'Downloading file list from R2...',
    },
    storing: {
      icon: <DatabaseOutlined />,
      title: 'Caching Data',
      description: 'Storing files in local database...',
    },
    indexing: {
      icon: <BuildOutlined />,
      title: 'Building Index',
      description: 'Creating folder structure...',
    },
    complete: {
      icon: <CheckCircleOutlined />,
      title: 'Ready!',
      description: 'Your files are ready to browse',
    },
  };

  const { icon, title, description } = phaseInfo[phase];

  // Calculate progress display
  let progressDisplay = null;
  if (phase === 'fetching' && processedFiles > 0) {
    progressDisplay = (
      <div className="sync-overlay-progress">
        <span className="sync-overlay-count">{processedFiles.toLocaleString()}</span>
        <span className="sync-overlay-label">files found</span>
      </div>
    );
  } else if (phase === 'storing' && totalFiles > 0) {
    progressDisplay = (
      <div className="sync-overlay-progress">
        <span className="sync-overlay-count">{totalFiles.toLocaleString()}</span>
        <span className="sync-overlay-label">files to cache</span>
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
