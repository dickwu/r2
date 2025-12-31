'use client';

import { Spin } from 'antd';
import { FolderOpenOutlined, CheckCircleOutlined } from '@ant-design/icons';
import { useSyncStore, FolderLoadPhase } from '../../stores/syncStore';

const folderPhaseConfig: Record<FolderLoadPhase, { icon: React.ReactNode; label: string }> = {
  idle: { icon: null, label: '' },
  loading: { icon: <FolderOpenOutlined />, label: 'Loading folder' },
  complete: { icon: <CheckCircleOutlined />, label: 'Complete' },
};

export default function FolderLoadProgress() {
  const isFolderLoading = useSyncStore((state) => state.isFolderLoading);
  const folderLoadPhase = useSyncStore((state) => state.folderLoadPhase);
  const folderLoadProgress = useSyncStore((state) => state.folderLoadProgress);

  if (!isFolderLoading || folderLoadPhase === 'idle' || folderLoadPhase === 'complete') {
    return null;
  }

  const { icon, label } = folderPhaseConfig[folderLoadPhase];
  const { pages, items } = folderLoadProgress;

  let progressText = '';
  if (pages > 0) {
    progressText = items > 0 ? `${items.toLocaleString()} items` : `page ${pages}`;
  }

  return (
    <span className="sync-progress">
      <Spin size="small" />
      <span className="sync-phase">
        {icon} {label}
      </span>
      {progressText && <span className="sync-count">{progressText}</span>}
    </span>
  );
}
