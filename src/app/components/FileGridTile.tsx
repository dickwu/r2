import { memo, useCallback, useState } from 'react';
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
import { StorageConfig } from '@/app/lib/r2cache';
import { usePreviewUrl } from '@/app/hooks/usePreviewUrl';
import { useListedObjectType } from '@/app/hooks/useListedObjectType';
import dayjs from 'dayjs';

const IMAGE_EXTENSIONS = ['jpg', 'jpeg', 'png', 'gif', 'webp', 'svg', 'bmp', 'ico'];
const VIDEO_EXTENSIONS = ['mp4', 'avi', 'mkv', 'mov', 'wmv', 'flv', 'webm', 'm4v'];

function getExt(name: string) {
  return name.toLowerCase().split('.').pop() || '';
}

function tileType(item: FileItem, ext: string): string {
  if (item.isFolder) return 'folder';
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

function TileIcon({ tone }: { tone: string }) {
  const style = { fontSize: 36 };
  switch (tone) {
    case 'folder':
      return <FolderOutlined style={style} />;
    case 'image':
      return <FileImageOutlined style={style} />;
    case 'video':
      return <VideoCameraOutlined style={style} />;
    case 'code':
      return <CodeOutlined style={style} />;
    case 'doc':
      return <FileTextOutlined style={style} />;
    case 'archive':
      return <FileZipOutlined style={style} />;
    default:
      return <FileOutlined style={style} />;
  }
}

/**
 * Grid thumbnail image. The tone-colored type icon stays visible until the
 * image has actually painted, then the image fades in over it; if the image
 * fails to load (revoked credentials, expired signature, offline) the tile
 * shows the icon instead of a broken-image glyph.
 */
function ThumbImage({
  src,
  alt,
  fallback,
}: {
  src: string;
  alt: string;
  fallback: React.ReactNode;
}) {
  const [loaded, setLoaded] = useState(false);
  const [failed, setFailed] = useState(false);
  if (failed) return <>{fallback}</>;
  return (
    <>
      {!loaded && fallback}
      <img
        className={['fg-thumb-img', loaded ? 'is-loaded' : ''].filter(Boolean).join(' ')}
        src={src}
        alt={alt}
        loading="lazy"
        onLoad={() => setLoaded(true)}
        onError={() => setFailed(true)}
      />
    </>
  );
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
  const listedType = useListedObjectType(storageConfig, item);
  // An extension-less object is shown as what its Content-Type says it is.
  const ext = listedType?.extension || getExt(item.name);
  const hasImage =
    !item.isFolder && (IMAGE_EXTENSIONS.includes(ext) || listedType?.kind === 'image');
  const hasVideo =
    !item.isFolder && (VIDEO_EXTENSIONS.includes(ext) || listedType?.kind === 'video');
  const tone = hasImage ? 'image' : hasVideo ? 'video' : tileType(item, ext);
  const fileUrl = usePreviewUrl(storageConfig, item.key, hasImage || hasVideo);

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
          {fileUrl && hasImage ? (
            <ThumbImage
              key={fileUrl}
              src={fileUrl}
              alt={item.name}
              fallback={<TileIcon tone={tone} />}
            />
          ) : fileUrl && hasVideo ? (
            <VideoThumbnail src={fileUrl} alt={item.name} />
          ) : (
            <TileIcon tone={tone} />
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
