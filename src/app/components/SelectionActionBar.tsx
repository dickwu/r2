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
  const countLabel =
    fileCount != null && fileCount !== selectedCount
      ? `${selectedCount} items (${fileCount.toLocaleString()} files)`
      : `${selectedCount} selected`;

  return (
    <div className="action-bar">
      <span className="action-bar-count">{countLabel}</span>
      <button className="btn btn-sm" onClick={onDownload}>
        <DownloadOutlined style={{ fontSize: 12 }} />
        Download
      </button>
      <button className="btn btn-sm" onClick={onMove}>
        <FolderOutlined style={{ fontSize: 12 }} />
        Move
      </button>
      <button className="btn btn-sm btn-danger" onClick={onDelete}>
        <DeleteOutlined style={{ fontSize: 12 }} />
        Delete
      </button>
      <span className="spacer" />
      <button className="btn btn-sm btn-ghost" onClick={onClear}>
        Clear
      </button>
    </div>
  );
}
