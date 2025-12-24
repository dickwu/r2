import { Button, Checkbox, Popconfirm } from 'antd';
import {
  FolderOutlined,
  DeleteOutlined,
  EditOutlined,
  CaretUpOutlined,
  CaretDownOutlined,
  MoreOutlined,
} from '@ant-design/icons';
import { Virtuoso } from 'react-virtuoso';
import { FileItem } from '../hooks/useR2Files';
import { formatBytes } from '../utils/formatBytes';
import { getFileIcon } from '../utils/fileIcon';

type SortOrder = 'asc' | 'desc' | null;

interface FolderMetadata {
  size: number | 'loading' | 'error';
  fileCount: number | null;
  totalFileCount: number | null;
  lastModified: string | null;
}

interface FileListViewProps {
  items: FileItem[];
  selectedKeys: Set<string>;
  metadata: Record<string, FolderMetadata>;
  sizeSort: SortOrder;
  modifiedSort: SortOrder;
  onItemClick: (item: FileItem) => void;
  onToggleSelection: (key: string) => void;
  onSelectAll: () => void;
  onClearSelection: () => void;
  onToggleSizeSort: () => void;
  onToggleModifiedSort: () => void;
  onDelete: (item: FileItem) => void;
  onRename: (item: FileItem) => void;
}

function formatDate(date: string): string {
  return new Date(date).toLocaleDateString(undefined, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
  });
}

export default function FileListView({
  items,
  selectedKeys,
  metadata,
  sizeSort,
  modifiedSort,
  onItemClick,
  onToggleSelection,
  onSelectAll,
  onClearSelection,
  onToggleSizeSort,
  onToggleModifiedSort,
  onDelete,
  onRename,
}: FileListViewProps) {
  const fileItems = items.filter((item) => !item.isFolder);
  
  return (
    <div style={{ height: '100%', display: 'flex', flexDirection: 'column' }}>
      {/* Header */}
      <div className="file-list-header">
        <span className="col-checkbox">
          <Checkbox
            indeterminate={selectedKeys.size > 0 && selectedKeys.size < fileItems.length}
            checked={selectedKeys.size > 0 && selectedKeys.size === fileItems.length}
            onChange={(e) => {
              if (e.target.checked) {
                onSelectAll();
              } else {
                onClearSelection();
              }
            }}
          />
        </span>
        <span className="col-name">Name</span>
        <span className="col-size sortable" onClick={onToggleSizeSort}>
          <span>Size</span>
          {sizeSort === 'asc' ? <CaretUpOutlined style={{ color: 'var(--text-secondary)' }} /> : null}
          {sizeSort === 'desc' ? <CaretDownOutlined style={{ color: 'var(--text-secondary)' }} /> : null}
          {sizeSort === null ? <MoreOutlined style={{ color: 'var(--text-secondary)' }} /> : null}
        </span>
        <span className="col-date sortable" onClick={onToggleModifiedSort}>
          Modified 
          {modifiedSort === 'asc' ? <CaretUpOutlined style={{ color: 'var(--text-secondary)' }} /> : null}
          {modifiedSort === 'desc' ? <CaretDownOutlined style={{ color: 'var(--text-secondary)' }} /> : null}
          {modifiedSort === null ? <MoreOutlined style={{ color: 'var(--text-secondary)' }} /> : null}
        </span>
        <span className="col-actions">Actions</span>
      </div>

      {/* Virtualized Items */}
      <Virtuoso
        style={{ flex: 1 }}
        data={items}
        itemContent={(index, item) => (
          <div
            className={`file-item ${item.isFolder ? 'folder' : 'file'} ${selectedKeys.has(item.key) ? 'selected' : ''}`}
            onClick={() => onItemClick(item)}
          >
            <span className="col-checkbox" onClick={(e) => e.stopPropagation()}>
              {!item.isFolder && (
                <Checkbox
                  checked={selectedKeys.has(item.key)}
                  onChange={() => onToggleSelection(item.key)}
                />
              )}
            </span>
            <span className="col-name">
              {item.isFolder ? (
                <FolderOutlined className="icon folder-icon" />
              ) : (
                getFileIcon(item.name)
              )}
              <span className="name">{item.name}</span>
            </span>
            <span className="col-size">
              {item.isFolder
                ? metadata[item.key]?.size === 'loading'
                  ? '...'
                  : metadata[item.key]?.size === 'error'
                    ? 'Error'
                    : typeof metadata[item.key]?.size === 'number'
                      ? formatBytes(metadata[item.key].size as number)
                      : '--'
                : formatBytes(item.size || 0)}
            </span>
            <span className="col-date">
              {item.isFolder
                ? metadata[item.key]?.lastModified
                  ? formatDate(metadata[item.key].lastModified!)
                  : '--'
                : item.lastModified
                  ? formatDate(item.lastModified)
                  : '--'}
            </span>
            <span className="col-actions" onClick={(e) => e.stopPropagation()}>
              {!item.isFolder && (
                <>
                  <Button
                    type="text"
                    size="small"
                    icon={<EditOutlined />}
                    onClick={() => onRename(item)}
                  />
                  <Popconfirm
                    title="Delete file"
                    description={`Are you sure you want to delete "${item.name}"?`}
                    onConfirm={() => onDelete(item)}
                    okText="Delete"
                    cancelText="Cancel"
                    okButtonProps={{ danger: true }}
                  >
                    <Button type="text" size="small" danger icon={<DeleteOutlined />} />
                  </Popconfirm>
                </>
              )}
            </span>
          </div>
        )}
      />
    </div>
  );
}
