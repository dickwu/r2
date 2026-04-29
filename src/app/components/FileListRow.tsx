/**
 * FileListRow — renders 5 sibling .fl-cell divs wrapped in `.fl-row`.
 * `.fl-row` is `display: contents`, so the 5 cells effectively participate
 * directly in the per-row grid defined by FileListView's VirtuosoItem
 * wrapper (display: grid; grid-template-columns: GRID_COLS). The .fl-row
 * element stays in the DOM for hover/selected state styling via the
 * `.fl-row:hover > .fl-cell` and `.fl-row.selected > .fl-cell` selectors.
 */
import {
  DownloadOutlined,
  EditOutlined,
  DeleteOutlined,
  SwapOutlined,
  CheckOutlined,
  MinusOutlined,
  FolderOutlined,
  FileImageOutlined,
  VideoCameraOutlined,
  CodeOutlined,
  FileTextOutlined,
  FileZipOutlined,
  FileOutlined,
} from '@ant-design/icons';
import { Tooltip } from 'antd';
import dayjs from 'dayjs';
import { FileItem } from '@/app/hooks/useR2Files';
import { formatBytes } from '@/app/utils/formatBytes';

type Density = 'compact' | 'default' | 'cozy';

interface FolderMetadata {
  size: number | 'loading' | 'error';
  fileCount: number | null;
  totalFileCount: number | null;
  lastModified: string | null;
}

interface FileListRowProps {
  item: FileItem;
  isSelected: boolean;
  density: Density;
  showFullPath: boolean;
  folderMeta?: FolderMetadata;
  onItemClick: (item: FileItem) => void;
  onToggleSelection: (key: string) => void;
  onDownload?: (item: FileItem) => void;
  onRename: (item: FileItem) => void;
  onDelete: (item: FileItem) => void;
  onFolderDownload?: (item: FileItem) => void;
  onFolderRename?: (item: FileItem) => void;
  onFolderDelete?: (item: FileItem) => void;
  onMove: (item: FileItem) => void;
}

function formatDate(date: string): string {
  return dayjs(date).format('YYYY-MM-DD');
}

function formatDateTime(date: string): string {
  return dayjs(date).format('YYYY-MM-DD HH:mm:ss');
}

/** Returns design tone class and antd icon for a file name */
function toneAndIcon(item: FileItem): {
  tone: string;
  Icon: React.ComponentType<{ style?: React.CSSProperties }>;
} {
  if (item.isFolder) return { tone: 'folder', Icon: FolderOutlined };
  const ext = item.name.toLowerCase().split('.').pop() || '';

  if (['jpg', 'jpeg', 'png', 'gif', 'webp', 'svg', 'bmp', 'ico'].includes(ext))
    return { tone: 'image', Icon: FileImageOutlined };
  if (['mp4', 'avi', 'mkv', 'mov', 'wmv', 'flv', 'webm', 'm4v'].includes(ext))
    return { tone: 'video', Icon: VideoCameraOutlined };
  if (
    [
      'js',
      'jsx',
      'ts',
      'tsx',
      'py',
      'java',
      'c',
      'cpp',
      'h',
      'cs',
      'php',
      'rb',
      'go',
      'rs',
      'swift',
      'kt',
      'html',
      'htm',
      'xml',
      'json',
      'yaml',
      'yml',
      'toml',
      'ini',
      'conf',
    ].includes(ext)
  )
    return { tone: 'code', Icon: CodeOutlined };
  if (
    [
      'pdf',
      'doc',
      'docx',
      'xls',
      'xlsx',
      'csv',
      'ppt',
      'pptx',
      'txt',
      'log',
      'md',
      'markdown',
    ].includes(ext)
  )
    return { tone: 'doc', Icon: FileTextOutlined };
  if (['zip', 'rar', '7z', 'tar', 'gz', 'bz2', 'xz'].includes(ext))
    return { tone: 'archive', Icon: FileZipOutlined };
  return { tone: '', Icon: FileOutlined };
}

/** Custom checkbox div with checked/indeterminate state */
export function FlCheckbox({
  checked,
  indeterminate,
  onClick,
}: {
  checked: boolean;
  indeterminate?: boolean;
  onClick: (e: React.MouseEvent) => void;
}) {
  const cls = ['fl-checkbox', checked ? 'checked' : '', indeterminate ? 'indeterminate' : '']
    .filter(Boolean)
    .join(' ');
  return (
    <div className={cls} onClick={onClick}>
      {indeterminate && !checked ? <MinusOutlined style={{ fontSize: 8 }} /> : null}
      {checked ? <CheckOutlined style={{ fontSize: 8 }} /> : null}
    </div>
  );
}

export default function FileListRow({
  item,
  isSelected,
  density,
  showFullPath,
  folderMeta,
  onItemClick,
  onToggleSelection,
  onDownload,
  onRename,
  onDelete,
  onFolderDownload,
  onFolderRename,
  onFolderDelete,
  onMove,
}: FileListRowProps) {
  const { tone, Icon } = toneAndIcon(item);
  const densityClass = density !== 'default' ? density : '';
  const rowClass = ['fl-row', densityClass, isSelected ? 'selected' : ''].filter(Boolean).join(' ');

  // Size display
  let sizeDisplay: React.ReactNode = '--';
  if (item.isFolder) {
    if (folderMeta?.size === 'loading') sizeDisplay = '...';
    else if (folderMeta?.size === 'error') sizeDisplay = 'Error';
    else if (typeof folderMeta?.size === 'number') sizeDisplay = formatBytes(folderMeta.size);
  } else {
    sizeDisplay = formatBytes(item.size || 0);
  }

  // Modified display
  let modDisplay: React.ReactNode = '--';
  if (item.isFolder) {
    if (folderMeta?.lastModified) {
      modDisplay = (
        <Tooltip title={formatDateTime(folderMeta.lastModified)}>
          <span>{formatDate(folderMeta.lastModified)}</span>
        </Tooltip>
      );
    }
  } else if (item.lastModified) {
    modDisplay = (
      <Tooltip title={formatDateTime(item.lastModified)}>
        <span>{formatDate(item.lastModified)}</span>
      </Tooltip>
    );
  }

  // Folder count meta
  const folderCount = folderMeta?.totalFileCount ?? folderMeta?.fileCount;

  const iconSize = density === 'compact' ? 11 : density === 'cozy' ? 15 : 13;

  return (
    <div className={rowClass} onClick={() => onItemClick(item)}>
      {/* Checkbox cell */}
      <div className="fl-cell" onClick={(e) => e.stopPropagation()}>
        <FlCheckbox
          checked={isSelected}
          onClick={(e) => {
            e.stopPropagation();
            onToggleSelection(item.key);
          }}
        />
      </div>

      {/* Name cell */}
      <div className="fl-cell fl-name">
        <div className={`fl-icon ${tone}`}>
          <Icon style={{ fontSize: iconSize }} />
        </div>
        <span className="fl-name-text">{showFullPath ? item.key : item.name}</span>
        {item.isFolder && folderCount != null && (
          <span className="fl-name-meta">{folderCount.toLocaleString()} items</span>
        )}
      </div>

      {/* Size cell */}
      <div className="fl-cell fl-mono">{sizeDisplay}</div>

      {/* Modified cell */}
      <div className="fl-cell fl-mono">{modDisplay}</div>

      {/* Actions cell */}
      <div
        className="fl-cell"
        style={{ justifyContent: 'flex-end' }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="fl-actions">
          {item.isFolder ? (
            <>
              {onFolderDownload && (
                <button
                  className="fl-act-btn"
                  title="Download folder"
                  onClick={() => onFolderDownload(item)}
                >
                  <DownloadOutlined style={{ fontSize: 12 }} />
                </button>
              )}
              {onFolderRename && (
                <button
                  className="fl-act-btn"
                  title="Rename folder"
                  onClick={() => onFolderRename(item)}
                >
                  <EditOutlined style={{ fontSize: 12 }} />
                </button>
              )}
              {onFolderDelete && (
                <button
                  className="fl-act-btn danger"
                  title="Delete folder"
                  onClick={() => onFolderDelete(item)}
                >
                  <DeleteOutlined style={{ fontSize: 12 }} />
                </button>
              )}
            </>
          ) : (
            <>
              {onDownload && (
                <button className="fl-act-btn" title="Download" onClick={() => onDownload(item)}>
                  <DownloadOutlined style={{ fontSize: 12 }} />
                </button>
              )}
              <button className="fl-act-btn" title="Move" onClick={() => onMove(item)}>
                <SwapOutlined style={{ fontSize: 12 }} />
              </button>
              <button className="fl-act-btn" title="Rename" onClick={() => onRename(item)}>
                <EditOutlined style={{ fontSize: 12 }} />
              </button>
              <button className="fl-act-btn danger" title="Delete" onClick={() => onDelete(item)}>
                <DeleteOutlined style={{ fontSize: 12 }} />
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
