import { memo, useCallback } from 'react';
import { Masonry, Card, Popconfirm, Button, Space } from 'antd';
import {
  FolderOutlined,
  FileOutlined,
  FileImageOutlined,
  PlaySquareOutlined,
  DeleteOutlined,
  EditOutlined,
} from '@ant-design/icons';
import { FileItem } from '../hooks/useR2Files';
import { FolderMetadata } from '../stores/folderSizeStore';
import VideoThumbnail from './VideoThumbnail';
import { formatBytes } from '../utils/formatBytes';

const IMAGE_EXTENSIONS = ['jpg', 'jpeg', 'png', 'gif', 'webp', 'svg', 'bmp', 'ico'];
const VIDEO_EXTENSIONS = ['mp4', 'webm', 'ogg', 'mov', 'm4v'];

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
  const domain = publicDomain.replace(/\/$/, '');
  return `https://${domain}/${key}`;
}

interface FileGridViewProps {
  items: FileItem[];
  onItemClick: (item: FileItem) => void;
  onDelete: (item: FileItem) => void;
  onRename: (item: FileItem) => void;
  publicDomain?: string;
  folderSizes?: Record<string, FolderMetadata>;
}

const FileCard = memo(function FileCard({
  item,
  publicDomain,
  onItemClick,
  onDelete,
  onRename,
  folderMetadata,
}: {
  item: FileItem;
  publicDomain?: string;
  onItemClick: (item: FileItem) => void;
  onDelete: (item: FileItem) => void;
  onRename: (item: FileItem) => void;
  folderMetadata?: FolderMetadata;
}) {
  const isImage = !item.isFolder && isImageFile(item.name);
  const isVideo = !item.isFolder && isVideoFile(item.name);
  const fileUrl = getFileUrl(item.key, publicDomain);
  const hasPreview = fileUrl && (isImage || isVideo);

  const handleClick = useCallback(() => {
    onItemClick(item);
  }, [onItemClick, item]);

  const handleDelete = useCallback(() => {
    return onDelete(item);
  }, [onDelete, item]);

  const handleRename = useCallback(() => {
    onRename(item);
  }, [onRename, item]);

  const stopPropagation = useCallback((e: React.MouseEvent) => {
    e.stopPropagation();
  }, []);

  return (
    <Card
      hoverable
      className={`grid-card ${item.isFolder ? 'folder' : 'file'} ${hasPreview ? 'has-preview' : ''}`}
      onClick={handleClick}
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
          ? folderMetadata?.size === 'loading'
            ? '...'
            : typeof folderMetadata?.size === 'number'
              ? formatBytes(folderMetadata.size)
              : 'Folder'
          : formatBytes(item.size || 0)}
      </div>
      {!item.isFolder && (
        <div className="grid-card-actions" onClick={stopPropagation}>
          <Space size={4}>
            <Button type="text" size="small" icon={<EditOutlined />} onClick={handleRename} />
            <Popconfirm
              title="Delete file"
              description={`Are you sure you want to delete "${item.name}"?`}
              onConfirm={handleDelete}
              okText="Delete"
              cancelText="Cancel"
              okButtonProps={{ danger: true }}
            >
              <Button type="text" size="small" danger icon={<DeleteOutlined />} />
            </Popconfirm>
          </Space>
        </div>
      )}
    </Card>
  );
});

export default memo(function FileGridView({
  items,
  onItemClick,
  onDelete,
  onRename,
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
              onItemClick={onItemClick}
              onDelete={onDelete}
              onRename={onRename}
              folderMetadata={item.isFolder ? folderSizes?.[item.key] : undefined}
            />
          ),
        }))}
      />
    </div>
  );
});
