import { useTranslation } from 'react-i18next';
import { DownloadOutlined, FolderOutlined, DeleteOutlined } from '@ant-design/icons';

interface SelectionActionBarProps {
  selectedCount: number;
  fileCount?: number;
  onDownload: () => void;
  onMove: () => void;
  onDelete: () => void;
  onClear: () => void;
}

export default function SelectionActionBar({
  selectedCount,
  fileCount,
  onDownload,
  onMove,
  onDelete,
  onClear,
}: SelectionActionBarProps) {
  const { t } = useTranslation();
  const countLabel =
    fileCount != null && fileCount !== selectedCount
      ? t('selection.itemsWithFiles', {
          count: selectedCount,
          files: fileCount.toLocaleString(),
        })
      : t('status.selected', { count: selectedCount });

  return (
    <div className="action-bar">
      <span className="action-bar-count">{countLabel}</span>
      <button className="btn btn-sm" onClick={onDownload}>
        <DownloadOutlined style={{ fontSize: 12 }} />
        {t('files.action.download')}
      </button>
      <button className="btn btn-sm" onClick={onMove}>
        <FolderOutlined style={{ fontSize: 12 }} />
        {t('files.action.move')}
      </button>
      <button className="btn btn-sm btn-danger" onClick={onDelete}>
        <DeleteOutlined style={{ fontSize: 12 }} />
        {t('files.action.delete')}
      </button>
      <span className="spacer" />
      <button className="btn btn-sm btn-ghost" onClick={onClear}>
        {t('common.clear')}
      </button>
    </div>
  );
}
