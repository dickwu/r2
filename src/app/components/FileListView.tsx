import { Button, Checkbox, Popconfirm } from 'antd';
import {
  FolderOutlined,
  FileOutlined,
  FileImageOutlined,
  FileTextOutlined,
  FilePdfOutlined,
  FileZipOutlined,
  FileExcelOutlined,
  FileWordOutlined,
  FilePptOutlined,
  FileMarkdownOutlined,
  CodeOutlined,
  VideoCameraOutlined,
  AudioOutlined,
  DeleteOutlined,
  EditOutlined,
  CaretUpOutlined,
  CaretDownOutlined,
} from '@ant-design/icons';
import { Virtuoso } from 'react-virtuoso';
import { FileItem } from '../hooks/useR2Files';
import { formatBytes } from '../utils/formatBytes';

type SortOrder = 'asc' | 'desc' | null;

interface FolderMetadata {
  size: number | 'loading' | 'error';
  fileCount: number | null;
  totalFileCount: number | null;
}

interface FileListViewProps {
  items: FileItem[];
  selectedKeys: Set<string>;
  metadata: Record<string, FolderMetadata>;
  sizeSort: SortOrder;
  onItemClick: (item: FileItem) => void;
  onToggleSelection: (key: string) => void;
  onSelectAll: () => void;
  onClearSelection: () => void;
  onToggleSizeSort: () => void;
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

function getFileIcon(fileName: string) {
  const ext = fileName.toLowerCase().split('.').pop() || '';
  
  // Image files
  if (['jpg', 'jpeg', 'png', 'gif', 'webp', 'svg', 'bmp', 'ico'].includes(ext)) {
    return <FileImageOutlined className="icon file-icon" />;
  }
  
  // Video files
  if (['mp4', 'avi', 'mkv', 'mov', 'wmv', 'flv', 'webm', 'm4v'].includes(ext)) {
    return <VideoCameraOutlined className="icon file-icon" />;
  }
  
  // Audio files
  if (['mp3', 'wav', 'ogg', 'flac', 'aac', 'm4a', 'wma'].includes(ext)) {
    return <AudioOutlined className="icon file-icon" />;
  }
  
  // Document files
  if (['pdf'].includes(ext)) {
    return <FilePdfOutlined className="icon file-icon" />;
  }
  
  if (['doc', 'docx'].includes(ext)) {
    return <FileWordOutlined className="icon file-icon" />;
  }
  
  if (['xls', 'xlsx', 'csv'].includes(ext)) {
    return <FileExcelOutlined className="icon file-icon" />;
  }
  
  if (['ppt', 'pptx'].includes(ext)) {
    return <FilePptOutlined className="icon file-icon" />;
  }
  
  // Code files
  if (['js', 'jsx', 'ts', 'tsx', 'py', 'java', 'c', 'cpp', 'h', 'cs', 'php', 'rb', 'go', 'rs', 'swift', 'kt'].includes(ext)) {
    return <CodeOutlined className="icon file-icon" />;
  }
  
  // Markup/config files
  if (['html', 'htm', 'xml', 'json', 'yaml', 'yml', 'toml', 'ini', 'conf'].includes(ext)) {
    return <CodeOutlined className="icon file-icon" />;
  }
  
  // Text files
  if (['txt', 'log'].includes(ext)) {
    return <FileTextOutlined className="icon file-icon" />;
  }
  
  // Markdown
  if (['md', 'markdown'].includes(ext)) {
    return <FileMarkdownOutlined className="icon file-icon" />;
  }
  
  // Archive files
  if (['zip', 'rar', '7z', 'tar', 'gz', 'bz2', 'xz'].includes(ext)) {
    return <FileZipOutlined className="icon file-icon" />;
  }
  
  // Default
  return <FileOutlined className="icon file-icon" />;
}

export default function FileListView({
  items,
  selectedKeys,
  metadata,
  sizeSort,
  onItemClick,
  onToggleSelection,
  onSelectAll,
  onClearSelection,
  onToggleSizeSort,
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
          Size
          {sizeSort === 'asc' && <CaretUpOutlined />}
          {sizeSort === 'desc' && <CaretDownOutlined />}
        </span>
        <span className="col-date">Modified</span>
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
              {item.lastModified ? formatDate(item.lastModified) : '--'}
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
