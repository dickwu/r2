import { memo, useCallback } from 'react';
import {
  FolderOutlined,
  FileImageOutlined,
  VideoCameraOutlined,
  CodeOutlined,
  FileTextOutlined,
  FileZipOutlined,
  FileOutlined,
  CheckOutlined,
} from '@ant-design/icons';
import { FileItem } from '@/app/hooks/useR2Files';
import { FolderMetadata } from '@/app/stores/folderSizeStore';
import { formatBytes } from '@/app/utils/formatBytes';
import VideoThumbnail from '@/app/components/VideoThumbnail';
import FileContextMenu from '@/app/components/FileContextMenu';
import { buildPublicUrl, StorageConfig } from '@/app/lib/r2cache';
import dayjs from 'dayjs';

const IMAGE_EXTENSIONS = ['jpg', 'jpeg', 'png', 'gif', 'webp', 'svg', 'bmp', 'ico'];
const VIDEO_EXTENSIONS = ['mp4', 'avi', 'mkv', 'mov', 'wmv', 'flv', 'webm', 'm4v'];

function getExt(name: string) {
  return name.toLowerCase().split('.').pop() || '';
}

function isImage(name: string) {
  return IMAGE_EXTENSIONS.includes(getExt(name));
}
function isVideo(name: string) {
  return VIDEO_EXTENSIONS.includes(getExt(name));
}

function tileType(item: FileItem): string {
  if (item.isFolder) return 'folder';
  const ext = getExt(item.name);
  if (IMAGE_EXTENSIONS.includes(ext)) return 'image';
  if (VIDEO_EXTENSIONS.includes(ext)) return 'video';
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
    return 'code';
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
    return 'doc';
  if (['zip', 'rar', '7z', 'tar', 'gz', 'bz2', 'xz'].includes(ext)) return 'archive';
  return '';
}

function TileIcon({ item }: { item: FileItem }) {
  if (item.isFolder) return <FolderOutlined style={{ fontSize: 36 }} />;
  const ext = getExt(item.name);
  if (IMAGE_EXTENSIONS.includes(ext)) return <FileImageOutlined style={{ fontSize: 36 }} />;
  if (VIDEO_EXTENSIONS.includes(ext)) return <VideoCameraOutlined style={{ fontSize: 36 }} />;
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
    return <CodeOutlined style={{ fontSize: 36 }} />;
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
    return <FileTextOutlined style={{ fontSize: 36 }} />;
  if (['zip', 'rar', '7z', 'tar', 'gz', 'bz2', 'xz'].includes(ext))
    return <FileZipOutlined style={{ fontSize: 36 }} />;
  return <FileOutlined style={{ fontSize: 36 }} />;
}

interface FileGridTileProps {
  item: FileItem;
  storageConfig?: StorageConfig | null;
  folderMetadata?: FolderMetadata;
  isSelected: boolean;
  showFullPath?: boolean;
  onItemClick: (item: FileItem) => void;
  onToggleSelection: (key: string) => void;
  onDelete: (item: FileItem) => void;
  onRename: (item: FileItem) => void;
  onDownload?: (item: FileItem) => void;
  onFolderDelete?: (item: FileItem) => void;
  onFolderDownload?: (item: FileItem) => void;
  onFolderRename?: (item: FileItem) => void;
  onFocus?: (item: FileItem) => void;
}

const FileGridTile = memo(function FileGridTile({
  item,
  storageConfig,
  folderMetadata,
  isSelected,
  showFullPath,
  onItemClick,
  onToggleSelection,
  onDelete,
  onRename,
  onDownload,
  onFolderDelete,
  onFolderDownload,
  onFolderRename,
  onFocus,
}: FileGridTileProps) {
  const tone = tileType(item);
  const hasImage = !item.isFolder && isImage(item.name);
  const hasVideo = !item.isFolder && isVideo(item.name);
  const fileUrl = storageConfig?.publicDomain ? buildPublicUrl(storageConfig, item.key) : null;
  const hasPreview = fileUrl && (hasImage || hasVideo);

  const handleClick = useCallback(() => {
    if (onFocus) onFocus(item);
    onItemClick(item);
  }, [onItemClick, onFocus, item]);

  const handleCheckClick = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      onToggleSelection(item.key);
    },
    [onToggleSelection, item.key]
  );

  // Size / meta string
  let sizeText = '';
  let dateText = '';
  if (item.isFolder) {
    if (folderMetadata?.size === 'loading') sizeText = '...';
    else if (folderMetadata?.size === 'error') sizeText = 'Error';
    else if (typeof folderMetadata?.size === 'number') sizeText = formatBytes(folderMetadata.size);
    else sizeText = 'Folder';
  } else {
    sizeText = formatBytes(item.size || 0);
    if (item.lastModified) dateText = dayjs(item.lastModified).format('MM-DD');
  }

  return (
    <FileContextMenu
      item={item}
      onDownload={onDownload}
      onRename={onRename}
      onDelete={onDelete}
      onFolderDownload={onFolderDownload}
      onFolderRename={onFolderRename}
      onFolderDelete={onFolderDelete}
    >
      <div
        className={['fg-tile', isSelected ? 'selected' : ''].filter(Boolean).join(' ')}
        onClick={handleClick}
      >
        {/* Checkbox overlay */}
        <div className="fg-tile-check" onClick={handleCheckClick}>
          <div className={['fl-checkbox', isSelected ? 'checked' : ''].filter(Boolean).join(' ')}>
            {isSelected && <CheckOutlined style={{ fontSize: 8 }} />}
          </div>
        </div>

        {/* Thumbnail / preview */}
        <div className={`fg-thumb ${tone}`}>
          {hasPreview ? (
            hasImage ? (
              <img className="fg-thumb-img" src={fileUrl} alt={item.name} loading="lazy" />
            ) : (
              <VideoThumbnail src={fileUrl} alt={item.name} />
            )
          ) : (
            <TileIcon item={item} />
          )}
        </div>

        {/* Name */}
        <div className="fg-name" title={showFullPath ? item.key : item.name}>
          {showFullPath ? item.key : item.name}
        </div>

        {/* Meta row */}
        <div className="fg-meta">
          <span>{sizeText}</span>
          {dateText && <span>{dateText}</span>}
        </div>
      </div>
    </FileContextMenu>
  );
});

export default FileGridTile;
