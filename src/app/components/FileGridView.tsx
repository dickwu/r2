import { memo, useCallback, CSSProperties, forwardRef } from 'react';
import { Card, Popconfirm, Button, Space, Checkbox } from 'antd';
import {
  FolderOutlined,
  FileOutlined,
  FileImageOutlined,
  PlaySquareOutlined,
  DeleteOutlined,
  EditOutlined,
  DownloadOutlined,
} from '@ant-design/icons';
import { VirtuosoGrid } from 'react-virtuoso';
import { FileItem } from '@/app/hooks/useR2Files';
import { FolderMetadata } from '@/app/stores/folderSizeStore';
import VideoThumbnail from '@/app/components/VideoThumbnail';
import { formatBytes } from '@/app/utils/formatBytes';
import { buildPublicUrl, StorageConfig } from '@/app/lib/r2cache';

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

interface FileGridViewProps {
  items: FileItem[];
  onItemClick: (item: FileItem) => void;
  onDelete: (item: FileItem) => void;
  onRename: (item: FileItem) => void;
  onDownload?: (item: FileItem) => void;
  onFolderDelete?: (item: FileItem) => void;
  onFolderDownload?: (item: FileItem) => void;
  onFolderRename?: (item: FileItem) => void;
  storageConfig?: StorageConfig | null;
  folderSizes?: Record<string, FolderMetadata>;
  selectedKeys?: Set<string>;
  onToggleSelection?: (key: string) => void;
  showFullPath?: boolean;
}

const FileCard = memo(function FileCard({
  item,
  storageConfig,
  onItemClick,
  onDelete,
  onRename,
  onDownload,
  onFolderDelete,
  onFolderDownload,
  onFolderRename,
  folderMetadata,
  isSelected,
  onToggleSelection,
  showFullPath,
}: {
  item: FileItem;
  storageConfig?: StorageConfig | null;
  onItemClick: (item: FileItem) => void;
  onDelete: (item: FileItem) => void;
  onRename: (item: FileItem) => void;
  onDownload?: (item: FileItem) => void;
  onFolderDelete?: (item: FileItem) => void;
  onFolderDownload?: (item: FileItem) => void;
  onFolderRename?: (item: FileItem) => void;
  folderMetadata?: FolderMetadata;
  isSelected?: boolean;
  onToggleSelection?: (key: string) => void;
  showFullPath?: boolean;
}) {
  const isImage = !item.isFolder && isImageFile(item.name);
  const isVideo = !item.isFolder && isVideoFile(item.name);
  const fileUrl = storageConfig?.publicDomain ? buildPublicUrl(storageConfig, item.key) : null;
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

  const handleFolderDelete = useCallback(() => {
    if (onFolderDelete) {
      onFolderDelete(item);
    }
  }, [onFolderDelete, item]);

  const handleFolderRename = useCallback(() => {
    if (onFolderRename) {
      onFolderRename(item);
    }
  }, [onFolderRename, item]);

  const handleFolderDownload = useCallback(() => {
    if (onFolderDownload) {
      onFolderDownload(item);
    }
  }, [onFolderDownload, item]);

  const handleDownload = useCallback(() => {
    if (onDownload) {
      onDownload(item);
    }
  }, [onDownload, item]);

  const stopPropagation = useCallback((e: React.MouseEvent) => {
    e.stopPropagation();
  }, []);

  const handleToggleSelection = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      if (onToggleSelection) {
        onToggleSelection(item.key);
      }
    },
    [onToggleSelection, item.key]
  );

  return (
    <Card
      hoverable
      className={`grid-card ${item.isFolder ? 'folder' : 'file'} ${hasPreview ? 'has-preview' : ''} ${isSelected ? 'selected' : ''}`}
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
      {onToggleSelection && (
        <div className="grid-card-checkbox" onClick={handleToggleSelection}>
          <Checkbox checked={isSelected} />
        </div>
      )}
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
      <div className="grid-card-name" title={showFullPath ? item.key : item.name}>
        {showFullPath ? item.key : item.name}
      </div>
      <div className="grid-card-meta">
        {item.isFolder
          ? (() => {
              if (folderMetadata?.size === 'loading') return '...';
              if (folderMetadata?.size === 'error') return 'Error';

              const sizeText =
                typeof folderMetadata?.size === 'number'
                  ? formatBytes(folderMetadata.size)
                  : 'Folder';

              const count = folderMetadata?.totalFileCount ?? folderMetadata?.fileCount;
              const countText =
                count != null ? `${count.toLocaleString()} file${count !== 1 ? 's' : ''}` : null;

              return countText ? (
                <>
                  <span>{sizeText}</span>
                  <br />
                  <span>{countText}</span>
                </>
              ) : (
                sizeText
              );
            })()
          : formatBytes(item.size || 0)}
      </div>
      {item.isFolder ? (
        (onFolderDownload || onFolderRename || onFolderDelete) && (
          <div className="grid-card-actions" onClick={stopPropagation}>
            <Space size={4}>
              {onFolderDownload && (
                <Button
                  type="text"
                  size="small"
                  icon={<DownloadOutlined />}
                  onClick={handleFolderDownload}
                />
              )}
              {onFolderRename && (
                <Button
                  type="text"
                  size="small"
                  icon={<EditOutlined />}
                  onClick={handleFolderRename}
                />
              )}
              {onFolderDelete && (
                <Button
                  type="text"
                  size="small"
                  danger
                  icon={<DeleteOutlined />}
                  onClick={handleFolderDelete}
                />
              )}
            </Space>
          </div>
        )
      ) : (
        <div className="grid-card-actions" onClick={stopPropagation}>
          <Space size={4}>
            {onDownload && (
              <Button
                type="text"
                size="small"
                icon={<DownloadOutlined />}
                onClick={handleDownload}
              />
            )}
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

// Grid container component
const GridList = forwardRef<HTMLDivElement, React.HTMLProps<HTMLDivElement>>(function GridList(
  { children, style, ...props },
  ref
) {
  return (
    <div
      ref={ref}
      {...props}
      style={{
        ...style,
        display: 'grid',
        gridTemplateColumns: 'repeat(auto-fill, minmax(180px, 1fr))',
        gap: '12px',
        padding: '12px',
      }}
    >
      {children}
    </div>
  );
});

// Grid item wrapper component
const GridItem = forwardRef<HTMLDivElement, React.HTMLProps<HTMLDivElement>>(function GridItem(
  { children, ...props },
  ref
) {
  return (
    <div ref={ref} {...props}>
      {children}
    </div>
  );
});

export default memo(function FileGridView({
  items,
  onItemClick,
  onDelete,
  onRename,
  onDownload,
  onFolderDelete,
  onFolderDownload,
  onFolderRename,
  storageConfig,
  folderSizes,
  selectedKeys,
  onToggleSelection,
  showFullPath,
}: FileGridViewProps) {
  return (
    <div className="file-grid" style={{ height: '100%' }}>
      <VirtuosoGrid
        style={{ height: '100%' }}
        data={items}
        components={{
          List: GridList,
          Item: GridItem,
        }}
        itemContent={(index, item) => (
          <FileCard
            item={item}
            storageConfig={storageConfig}
            onItemClick={onItemClick}
            onDelete={onDelete}
            onRename={onRename}
            onDownload={onDownload}
            onFolderDelete={onFolderDelete}
            onFolderDownload={onFolderDownload}
            onFolderRename={onFolderRename}
            folderMetadata={item.isFolder ? folderSizes?.[item.key] : undefined}
            isSelected={selectedKeys?.has(item.key)}
            onToggleSelection={onToggleSelection}
            showFullPath={showFullPath}
          />
        )}
      />
    </div>
  );
});
