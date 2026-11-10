/**
 * ColumnPreview — the last column of the column view when a file is selected:
 * a thumbnail (images and videos reuse the grid's cached thumbnails), the
 * file's name and details, and Open / Download actions.
 */
import { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import {
  DownloadOutlined,
  EyeOutlined,
  FileImageOutlined,
  VideoCameraOutlined,
} from '@ant-design/icons';
import dayjs from 'dayjs';
import type { FileItem } from '@/app/hooks/useR2Files';
import type { StorageConfig } from '@/app/lib/r2cache';
import type { ThumbnailObject } from '@/app/lib/thumbnailIdentity';
import { usePreviewUrl } from '@/app/hooks/usePreviewUrl';
import { useListedObjectType } from '@/app/hooks/useListedObjectType';
import { ImageThumbnail } from '@/app/components/FileGridTile';
import VideoThumbnail from '@/app/components/VideoThumbnail';
import { toneAndIcon } from '@/app/components/FileListRow';
import { formatBytes } from '@/app/utils/formatBytes';

interface ColumnPreviewProps {
  item: FileItem;
  config: StorageConfig | null;
  onOpen: () => void;
  onDownload?: (item: FileItem) => void;
}

const TONE_TYPE: Record<string, string> = {
  image: 'image',
  video: 'video',
  code: 'code',
  doc: 'doc',
  archive: 'archive',
};

export default function ColumnPreview({ item, config, onOpen, onDownload }: ColumnPreviewProps) {
  const { t } = useTranslation();
  const listedType = useListedObjectType(config, item);
  const base = toneAndIcon(item);
  // An extension-less object is shown as what its Content-Type says it is.
  const tone =
    listedType?.kind === 'image' || listedType?.kind === 'video' ? listedType.kind : base.tone;
  const Icon =
    tone === base.tone ? base.Icon : tone === 'image' ? FileImageOutlined : VideoCameraOutlined;
  const hasMedia = tone === 'image' || tone === 'video';
  const url = usePreviewUrl(config, item.key, hasMedia);
  const object = useMemo<ThumbnailObject | null>(
    () =>
      config
        ? {
            accountId: config.accountId,
            bucket: config.bucket,
            key: item.key,
            size: item.size,
            lastModified: item.lastModified || undefined,
          }
        : null,
    [config, item.key, item.size, item.lastModified]
  );

  const icon = <Icon style={{ fontSize: 40 }} />;
  const type = TONE_TYPE[tone] ?? 'file';

  return (
    <div className="cv-preview">
      <div className={`fg-thumb ${tone} cv-preview-thumb`} onDoubleClick={onOpen}>
        {url && tone === 'image' ? (
          <ImageThumbnail src={url} alt={item.name} object={object} fallback={icon} />
        ) : url && tone === 'video' ? (
          <VideoThumbnail src={url} alt={item.name} object={object} />
        ) : (
          icon
        )}
      </div>

      <div className="cv-preview-name">{item.name}</div>

      <div className="cv-preview-actions">
        <button className="btn btn-sm btn-primary" onClick={onOpen}>
          <EyeOutlined style={{ fontSize: 12 }} /> {t('columns.open')}
        </button>
        {onDownload && (
          <button className="btn btn-sm" onClick={() => onDownload(item)}>
            <DownloadOutlined style={{ fontSize: 12 }} /> {t('files.action.download')}
          </button>
        )}
      </div>

      <div className="cv-preview-details">
        <div className="kv">
          <span className="kv-k">{t('files.col.size')}</span>
          <span className="kv-v">{formatBytes(item.size || 0)}</span>
        </div>
        <div className="kv">
          <span className="kv-k">{t('files.col.modified')}</span>
          <span className="kv-v">
            {item.lastModified ? dayjs(item.lastModified).format('YYYY-MM-DD HH:mm') : '—'}
          </span>
        </div>
        <div className="kv">
          <span className="kv-k">{t('inspector.type')}</span>
          <span className="kv-v">{t(`inspector.types.${type}`, { defaultValue: type })}</span>
        </div>
      </div>
    </div>
  );
}
