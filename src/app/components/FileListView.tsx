import { Button, Checkbox, Popconfirm, Tooltip } from 'antd';
import {
  FolderOutlined,
  DeleteOutlined,
  EditOutlined,
  CaretUpOutlined,
  CaretDownOutlined,
  MoreOutlined,
} from '@ant-design/icons';
import { Virtuoso } from 'react-virtuoso';
import dayjs from 'dayjs';
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
  showFullPath?: boolean; // Show full path instead of just name (for search results)
  onItemClick: (item: FileItem) => void;
  onToggleSelection: (key: string) => void;
  onSelectAll: () => void;
  onClearSelection: () => void;
  onToggleSizeSort: () => void;
  onToggleModifiedSort: () => void;
  onDelete: (item: FileItem) => void;
  onRename: (item: FileItem) => void;
  onFolderDelete?: (item: FileItem) => void;
}

function formatDate(date: string): string {
  return dayjs(date).format('YYYY-MM-DD');
}

function formatDateTime(date: string): string {
  return dayjs(date).format('YYYY-MM-DD HH:mm:ss');
}

export default function FileListView({
  items,
  selectedKeys,
  metadata,
  sizeSort,
  modifiedSort,
  showFullPath = false,
  onItemClick,
  onToggleSelection,
  onSelectAll,
  onClearSelection,
  onToggleSizeSort,
  onToggleModifiedSort,
  onDelete,
  onRename,
  onFolderDelete,
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
          {sizeSort === 'asc' ? (
            <CaretUpOutlined style={{ color: 'var(--text-secondary)' }} />
          ) : null}
          {sizeSort === 'desc' ? (
            <CaretDownOutlined style={{ color: 'var(--text-secondary)' }} />
          ) : null}
          {sizeSort === null ? <MoreOutlined style={{ color: 'var(--text-secondary)' }} /> : null}
        </span>
        <span className="col-date sortable" onClick={onToggleModifiedSort}>
          Modified
          {modifiedSort === 'asc' ? (
            <CaretUpOutlined style={{ color: 'var(--text-secondary)' }} />
          ) : null}
          {modifiedSort === 'desc' ? (
            <CaretDownOutlined style={{ color: 'var(--text-secondary)' }} />
          ) : null}
          {modifiedSort === null ? (
            <MoreOutlined style={{ color: 'var(--text-secondary)' }} />
          ) : null}
        </span>
        <span className="col-actions">Actions</span>
      </div>

      {/* Virtualized Items */}
      <Virtuoso
        style={{ flex: 1 }}
        data={items}
        itemContent={(index, item) => {
          const folderMeta = item.isFolder ? metadata[item.key] : undefined;
          const folderCount = folderMeta?.totalFileCount ?? folderMeta?.fileCount;
          const folderCountTooltip =
            folderCount != null
              ? `${folderCount.toLocaleString()} file${folderCount !== 1 ? 's' : ''}`
              : undefined;

          return (
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
                <Tooltip title={showFullPath ? item.key : undefined}>
                  <span className="name">{showFullPath ? item.key : item.name}</span>
                </Tooltip>
              </span>
              <span className="col-size">
                {item.isFolder ? (
                  folderMeta?.size === 'loading' ? (
                    '...'
                  ) : folderMeta?.size === 'error' ? (
                    'Error'
                  ) : typeof folderMeta?.size === 'number' ? (
                    <Tooltip title={folderCountTooltip}>
                      <span>{formatBytes(folderMeta.size as number)}</span>
                    </Tooltip>
                  ) : (
                    '--'
                  )
                ) : (
                  formatBytes(item.size || 0)
                )}
              </span>
              <span className="col-date">
                {item.isFolder ? (
                  folderMeta?.lastModified ? (
                    <Tooltip title={formatDateTime(folderMeta.lastModified)}>
                      <span>{formatDate(folderMeta.lastModified)}</span>
                    </Tooltip>
                  ) : (
                    '--'
                  )
                ) : item.lastModified ? (
                  <Tooltip title={formatDateTime(item.lastModified)}>
                    <span>{formatDate(item.lastModified)}</span>
                  </Tooltip>
                ) : (
                  '--'
                )}
              </span>
              <span className="col-actions" onClick={(e) => e.stopPropagation()}>
                {item.isFolder ? (
                  onFolderDelete && (
                    <Tooltip title="Delete folder">
                      <Button
                        type="text"
                        size="small"
                        danger
                        icon={<DeleteOutlined />}
                        onClick={() => onFolderDelete(item)}
                      />
                    </Tooltip>
                  )
                ) : (
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
          );
        }}
      />
    </div>
  );
}
