import { Masonry, Card, Popconfirm, Button } from 'antd';
import {
  FolderOutlined,
  FileOutlined,
  FileImageOutlined,
  PlaySquareOutlined,
  DeleteOutlined,
} from '@ant-design/icons';
import { FileItem } from '../hooks/useR2Files';
import VideoThumbnail from './VideoThumbnail';

const IMAGE_EXTENSIONS = ['jpg', 'jpeg', 'png', 'gif', 'webp', 'svg', 'bmp', 'ico'];
const VIDEO_EXTENSIONS = ['mp4', 'webm', 'ogg', 'mov', 'm4v'];

function formatBytes(bytes: number): string {
  if (bytes === 0) return '0 B';
  const k = 1024;
  const sizes = ['B', 'KB', 'MB', 'GB'];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + ' ' + sizes[i];
}

function getFileExtension(filename: string): string {
  return filename.split('.').pop()?.toLowerCase() || '';
}

function isImageFile(filename: string): boolean {
  return IMAGE_EXTENSIONS.includes(getFileExtension(filename));
}

function isVideoFile(filename: string): boolean {
  return VIDEO_EXTENSIONS.includes(getFileExtension(filename));
}

function getFileUrl(key: string, publicDomain?: string): string | null {
  if (!publicDomain) return null;
  return `${publicDomain.replace(/\/$/, '')}/${key}`;
}

type FolderSizeState = number | 'loading' | 'error';

interface FileGridViewProps {
  items: FileItem[];
  onItemClick: (item: FileItem) => void;
  onDelete: (item: FileItem) => void;
  publicDomain?: string;
  folderSizes?: Record<string, FolderSizeState>;
}

function FileCard({
  item,
  publicDomain,
  onClick,
  onDelete,
  folderSize,
}: {
  item: FileItem;
  publicDomain?: string;
  onClick: () => void;
  onDelete: () => void;
  folderSize?: FolderSizeState;
}) {
  const isImage = !item.isFolder && isImageFile(item.name);
  const isVideo = !item.isFolder && isVideoFile(item.name);
  const fileUrl = getFileUrl(item.key, publicDomain);
  const hasPreview = fileUrl && (isImage || isVideo);

  return (
    <Card
      hoverable
      className={`grid-card ${item.isFolder ? 'folder' : 'file'} ${hasPreview ? 'has-preview' : ''}`}
      onClick={onClick}
      cover={
        hasPreview ? (
          isImage ? (
            <div className="grid-card-preview">
              <img src={fileUrl} alt={item.name} loading="lazy" />
            </div>
          ) : (
            <div className="grid-card-preview video">
              <VideoThumbnail src={fileUrl} alt={item.name} />
            </div>
          )
        ) : undefined
      }
    >
      {!hasPreview && (
        <div className="grid-card-icon">
          {item.isFolder ? (
            <FolderOutlined className="folder-icon" />
          ) : isImage ? (
            <FileImageOutlined className="image-icon" />
          ) : isVideo ? (
            <PlaySquareOutlined className="video-icon" />
          ) : (
            <FileOutlined className="file-icon" />
          )}
        </div>
      )}
      <div className="grid-card-name" title={item.name}>
        {item.name}
      </div>
      <div className="grid-card-meta">
        {item.isFolder
          ? folderSize === 'loading'
            ? '...'
            : typeof folderSize === 'number'
              ? formatBytes(folderSize)
              : 'Folder'
          : formatBytes(item.size || 0)}
      </div>
      {!item.isFolder && (
        <div className="grid-card-actions" onClick={(e) => e.stopPropagation()}>
          <Popconfirm
            title="Delete file"
            description={`Are you sure you want to delete "${item.name}"?`}
            onConfirm={onDelete}
            okText="Delete"
            cancelText="Cancel"
            okButtonProps={{ danger: true }}
          >
            <Button type="text" size="small" danger icon={<DeleteOutlined />} />
          </Popconfirm>
        </div>
      )}
    </Card>
  );
}

export default function FileGridView({
  items,
  onItemClick,
  onDelete,
  publicDomain,
  folderSizes,
}: FileGridViewProps) {
  return (
    <div className="file-grid">
      <Masonry
        columns={{ xs: 2, sm: 3, md: 4, lg: 5, xl: 6 }}
        gutter={12}
        items={items.map((item) => ({
          key: item.key,
          data: item,
          children: (
            <FileCard
              item={item}
              publicDomain={publicDomain}
              onClick={() => onItemClick(item)}
              onDelete={() => onDelete(item)}
              folderSize={item.isFolder ? folderSizes?.[item.key] : undefined}
            />
          ),
        }))}
      />
    </div>
  );
}
