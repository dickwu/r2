'use client';

import { useTranslation } from 'react-i18next';
import { Spin } from 'antd';
import { FolderOpenOutlined, CheckCircleOutlined } from '@ant-design/icons';
import { useSyncStore, FolderLoadPhase } from '@/app/stores/syncStore';

const folderPhaseConfig: Record<FolderLoadPhase, { icon: React.ReactNode; label: string }> = {
  idle: { icon: null, label: '' },
  loading: { icon: <FolderOpenOutlined />, label: 'status.loadingFolder' },
  complete: { icon: <CheckCircleOutlined />, label: 'status.complete' },
};

export default function FolderLoadProgress() {
  const isFolderLoading = useSyncStore((state) => state.isFolderLoading);
  const folderLoadPhase = useSyncStore((state) => state.folderLoadPhase);
  const folderLoadProgress = useSyncStore((state) => state.folderLoadProgress);
  const { t } = useTranslation();

  if (!isFolderLoading || folderLoadPhase === 'idle' || folderLoadPhase === 'complete') {
    return null;
  }

  const { icon, label } = folderPhaseConfig[folderLoadPhase];
  const { pages, items } = folderLoadProgress;

  let progressText = '';
  if (pages > 0) {
    progressText =
      items > 0
        ? t('status.items', { count: items, formatted: items.toLocaleString() })
        : t('status.page', { page: pages });
  }

  return (
    <span className="sync-progress">
      <Spin size="small" />
      <span className="sync-phase">
        {icon} {t(label)}
      </span>
      {progressText && <span className="sync-count">{progressText}</span>}
    </span>
  );
}
