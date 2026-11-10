'use client';

import { t as i18nT } from '@/i18n';
import { useTranslation } from 'react-i18next';
import { useState, useEffect, useCallback, useMemo } from 'react';
import { Image, Segmented, Spin, App, Tooltip } from 'antd';
import {
  LinkOutlined,
  CopyOutlined,
  FileOutlined,
  FileImageOutlined,
  PlaySquareOutlined,
  SoundOutlined,
  FilePdfOutlined,
  FileTextOutlined,
  LeftOutlined,
  RightOutlined,
  EditOutlined,
} from '@ant-design/icons';
import { openUrl } from '@tauri-apps/plugin-opener';
import dynamic from 'next/dynamic';
import {
  buildPublicUrl,
  generateSignedUrl,
  hasSigningCredentials,
  isBucketPublic,
  StorageConfig,
} from '@/app/lib/r2cache';
import { TEXT_EXTENSIONS } from '@/app/components/preview/TextViewer';
import BlobImage from '@/app/components/preview/BlobImage';
import { getImageBlobType, type PreviewTarget } from '@/app/components/preview/previewKind';
import { usePreviewTarget, type PreviewSource } from '@/app/components/preview/usePreviewTarget';
import { useVideoThumbnail } from '@/app/hooks/useVideoThumbnail';
import { usePreviewStore } from '@/app/stores/previewStore';
import Modal from '@/app/components/ui/Modal';
import FullscreenButton from '@/app/components/ui/FullscreenButton';
import { useFullscreen, useWindowFullscreen } from '@/app/components/ui/useFullscreen';
import { getEditability, type Editability } from '@/app/components/editor/editability';

const TextViewer = dynamic(() => import('@/app/components/preview/TextViewer'), { ssr: false });
// The viewer bundles every renderer preset (Office, PDF, CAD, archives, …), so
// it is only pulled in when a file actually needs it.
const FileViewerPane = dynamic(() => import('@/app/components/preview/FileViewerPane'), {
  ssr: false,
  loading: () => <Spin />,
});
// Monaco, the Markdown renderer and the image tools load when editing starts.
const FileEditorModal = dynamic(() => import('@/app/components/editor/FileEditorModal'), {
  ssr: false,
});

function fileTypeIcon({ kind, extension }: PreviewTarget, size = 18): React.ReactNode {
  const style = { fontSize: size };
  switch (kind) {
    case 'image':
      return <FileImageOutlined style={style} />;
    case 'video':
      return <PlaySquareOutlined style={style} />;
    case 'audio':
      return <SoundOutlined style={style} />;
    case 'text':
      return <FileTextOutlined style={style} />;
    default:
      return extension === 'pdf' ? (
        <FilePdfOutlined style={style} />
      ) : (
        <FileOutlined style={style} />
      );
  }
}

function placeholderIcon(target: PreviewTarget) {
  return fileTypeIcon(target, 40);
}

const KIND_LABELS: Record<PreviewTarget['kind'], string> = {
  image: 'image',
  video: 'video',
  audio: 'audio',
  text: 'text',
  viewer: 'file',
};

/** Short name of the format a preview renders, e.g. "PDF". */
function formatLabel({ kind, extension }: PreviewTarget): string {
  return extension ? extension.toUpperCase() : i18nT(`preview.kind.${KIND_LABELS[kind]}`);
}

interface FilePreviewModalProps {
  config?: StorageConfig | null;
  onCredentialsUpdate?: () => void;
  onFileUpdated?: () => void;
}

export default function FilePreviewModal({ config, onFileUpdated }: FilePreviewModalProps) {
  const { message } = App.useApp();
  const { t } = useTranslation();
  const { file, files, close, navigate, editing, startEditing, stopEditing, markSaved, revision } =
    usePreviewStore();
  const isOpen = !!file;
  // The editor, when open, owns the keys (its own Escape, no F while typing).
  const { fullscreen, toggle: toggleFullscreen } = useFullscreen({
    active: isOpen && !editing,
    shortcut: true,
  });
  useWindowFullscreen(isOpen && fullscreen);
  // Kept with the key it was made for: while the next file's URL is being
  // signed, that file must not preview (or probe) the previous one's URL.
  const [objectUrl, setObjectUrl] = useState<{ key: string; url: string | null } | null>(null);
  const signedUrl = file && objectUrl?.key === file.key ? objectUrl.url : null;
  const [loading, setLoading] = useState(false);
  const [windowSize, setWindowSize] = useState({ width: 0, height: 0 });

  useEffect(() => {
    if (typeof window === 'undefined') return;
    const updateSize = () =>
      setWindowSize({ width: window.innerWidth, height: window.innerHeight });
    updateSize();
    window.addEventListener('resize', updateSize);
    return () => window.removeEventListener('resize', updateSize);
  }, []);

  const previewableFiles = useMemo(() => files.filter((f) => !f.isFolder), [files]);
  const currentIndex = useMemo(
    () => (file ? previewableFiles.findIndex((f) => f.key === file.key) : -1),
    [file, previewableFiles]
  );

  // Keyboard navigation: arrow keys to switch files
  useEffect(() => {
    if (!isOpen || editing || previewableFiles.length <= 1) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      // A renderer that owns the key (slideshow, comic pages, media seek)
      // has already called preventDefault by the time it reaches window.
      if (e.defaultPrevented) return;
      // The file viewer renders inside a shadow root, which retargets
      // `e.target` to its host; the composed path still reaches the real
      // element (its search box, the scrolled document) through the boundary.
      const path = (e.composedPath?.() ?? [e.target]).filter(
        (node): node is HTMLElement => node instanceof HTMLElement
      );
      const tag = path[0]?.tagName;
      if (tag === 'INPUT' || tag === 'TEXTAREA' || path[0]?.isContentEditable) return;
      if (e.key === 'ArrowLeft') {
        e.preventDefault();
        navigate('prev');
      } else if (e.key === 'ArrowRight') {
        e.preventDefault();
        navigate('next');
      } else if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
        const isScrollable = path.some(
          (el) =>
            el.tagName === 'PRE' || el.tagName === 'CODE' || el.hasAttribute('data-scrollable')
        );
        if (!isScrollable) {
          e.preventDefault();
          navigate(e.key === 'ArrowUp' ? 'prev' : 'next');
        }
      }
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [isOpen, editing, previewableFiles.length, navigate]);

  const hasCredentials = hasSigningCredentials(config);
  const isPublic = isBucketPublic(config);
  const needsCredentials = !isPublic && !hasCredentials;
  const canEdit = !!(config?.bucket && hasCredentials);

  const preview = usePreviewTarget(file, signedUrl, TEXT_EXTENSIONS);
  const editability = useMemo(
    () => (file ? getEditability(file.name, preview.target, file.size) : null),
    [file, preview.target]
  );
  // The editor keeps the editability it opened with: the preview's content
  // detection settling mid-edit must not swap or unmount it.
  const [editingAs, setEditingAs] = useState<Extract<Editability, { editable: true }> | null>(null);
  const handleSaved = useCallback(
    (size: number) => {
      markSaved(size);
      onFileUpdated?.();
    },
    [markSaved, onFileUpdated]
  );

  const previewKey = isOpen && file ? file.key : null;
  if (!previewKey && objectUrl !== null) {
    setObjectUrl(null);
  }
  if (previewKey && isPublic && config) {
    const url = buildPublicUrl(config, previewKey);
    if (objectUrl?.key !== previewKey || objectUrl.url !== url) {
      setObjectUrl({ key: previewKey, url });
    }
  } else if (previewKey && (!config || (!isPublic && !hasCredentials))) {
    if (objectUrl !== null && objectUrl.key !== previewKey) {
      setObjectUrl(null);
    }
  }

  const needsSignature = Boolean(previewKey && config && hasCredentials && !isPublic);
  const [signKey, setSignKey] = useState<string | null>(null);
  if (needsSignature && previewKey && signKey !== previewKey) {
    setSignKey(previewKey);
    setLoading(true);
  } else if (!needsSignature && signKey !== null) {
    setSignKey(null);
  }

  useEffect(() => {
    if (!needsSignature || !previewKey || !config) return;
    let stale = false;
    generateSignedUrl(config, previewKey)
      .then((url) => {
        if (!stale) setObjectUrl({ key: previewKey, url });
      })
      .catch(() => {
        if (!stale) setObjectUrl({ key: previewKey, url: null });
      })
      .finally(() => {
        if (!stale) setLoading(false);
      });
    return () => {
      stale = true;
    };
  }, [needsSignature, previewKey, config]);

  // The same identity as the grid tile, so the poster comes from the disk cache.
  const posterObject = useMemo(
    () =>
      file && config
        ? {
            accountId: config.accountId,
            bucket: config.bucket,
            key: file.key,
            size: file.size,
            lastModified: file.lastModified || undefined,
          }
        : null,
    [file, config]
  );
  const { thumbnailSrc: videoPoster } = useVideoThumbnail(
    file && preview.target.kind === 'video' ? signedUrl : null,
    { object: posterObject }
  );

  if (!file) return null;

  const fileUrl = signedUrl;
  const { target } = preview;
  const { kind } = target;
  const isImage = kind === 'image';
  const isVideo = kind === 'video';
  const isAudio = kind === 'audio';
  const isText = kind === 'text';

  const getModalWidth = () => {
    const width = windowSize.width || (typeof window !== 'undefined' ? window.innerWidth : 1024);
    if (width < 640) return '95%';
    if (width < 1024) return '90%';
    return 1100;
  };

  const getPreviewMaxHeight = () => {
    const height = windowSize.height || (typeof window !== 'undefined' ? window.innerHeight : 768);
    // Fullscreen gives the content everything but the header and footer.
    if (fullscreen) return `${Math.max(height - 150, 240)}px`;
    return `${Math.min(height * 0.55, 700)}px`;
  };

  async function handleCopyUrl() {
    if (!fileUrl) {
      message.warning(t('preview.noDomain'));
      return;
    }
    try {
      await navigator.clipboard.writeText(fileUrl);
      message.success(t('preview.urlCopied'));
    } catch {
      message.error(t('preview.copyUrlFailed'));
    }
  }

  async function handleOpenUrl() {
    if (!fileUrl) {
      message.warning(t('preview.noDomain'));
      return;
    }
    try {
      await openUrl(fileUrl);
    } catch {
      message.error(t('preview.openUrlFailed'));
    }
  }

  // Build file size + modified subtitle
  const fileSizeStr = file.size
    ? file.size < 1024
      ? `${file.size} B`
      : file.size < 1024 * 1024
        ? `${(file.size / 1024).toFixed(1)} KB`
        : `${(file.size / (1024 * 1024)).toFixed(1)} MB`
    : null;
  const modifiedStr = file.lastModified
    ? new Date(file.lastModified).toLocaleDateString(undefined, {
        year: 'numeric',
        month: 'short',
        day: 'numeric',
      })
    : null;
  const subtitle = [fileSizeStr, modifiedStr].filter(Boolean).join(' · ') || undefined;

  const imageBlobType = getImageBlobType(target);
  const imageStyle = {
    maxHeight: getPreviewMaxHeight(),
    maxWidth: '100%',
    objectFit: 'contain',
  } as const;

  // Navigation counter
  const navCounter =
    previewableFiles.length > 1 ? `${currentIndex + 1} / ${previewableFiles.length}` : undefined;

  const footer = (
    <>
      {/* Left: copy URL */}
      <span className="left" style={{ display: 'flex', gap: 6 }}>
        <button className="btn" onClick={handleOpenUrl} disabled={!fileUrl}>
          <LinkOutlined /> {t('preview.openUrl')}
        </button>
        <button className="btn" onClick={handleCopyUrl} disabled={!fileUrl}>
          <CopyOutlined /> {t('preview.copyUrl')}
        </button>
      </span>

      {/* Navigation */}
      {previewableFiles.length > 1 && (
        <span style={{ display: 'flex', alignItems: 'center', gap: 4 }}>
          <button className="btn" onClick={() => navigate('prev')} style={{ padding: '0 10px' }}>
            <LeftOutlined />
          </button>
          <span
            style={{ fontSize: 12, color: 'var(--text-muted)', minWidth: 48, textAlign: 'center' }}
          >
            {navCounter}
          </span>
          <button className="btn" onClick={() => navigate('next')} style={{ padding: '0 10px' }}>
            <RightOutlined />
          </button>
        </span>
      )}

      {editability && (
        <Tooltip
          title={
            !canEdit
              ? needsCredentials || !hasCredentials
                ? t('preview.editNeedsKeys')
                : t('preview.chooseBucket')
              : editability.editable
                ? t('preview.editThis')
                : editability.reason
          }
        >
          <button
            className="btn"
            onClick={() => {
              if (!editability.editable) return;
              setEditingAs(editability);
              startEditing();
            }}
            disabled={!canEdit || !editability.editable}
          >
            <EditOutlined /> {t('preview.edit')}
          </button>
        </Tooltip>
      )}

      <button className="btn btn-primary" onClick={close}>
        {t('common.close')}
      </button>
    </>
  );

  const modalWidth = getModalWidth();

  return (
    <>
      <Modal
        open={isOpen}
        onClose={close}
        title={file.name}
        subtitle={subtitle}
        icon={fileTypeIcon(target)}
        width={typeof modalWidth === 'number' ? modalWidth : undefined}
        footer={footer}
        fullscreen={fullscreen}
        headerActions={
          <FullscreenButton fullscreen={fullscreen} onToggle={toggleFullscreen} shortcutHint />
        }
      >
        {/* The name and the content disagree on how to render it: let the user pick. */}
        {preview.choices && (
          <div
            style={{
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'center',
              flexWrap: 'wrap',
              gap: 10,
              marginBottom: 12,
              fontSize: 12.5,
              color: 'var(--text-muted)',
            }}
          >
            <span>
              {t('preview.mismatch', {
                content: preview.choices.contentName,
                name: formatLabel(preview.choices.name),
              })}
            </span>
            <Segmented<PreviewSource>
              size="small"
              value={preview.source}
              onChange={preview.setSource}
              options={[
                {
                  value: 'name',
                  label: t('preview.byName', { format: formatLabel(preview.choices.name) }),
                },
                {
                  value: 'content',
                  label: t('preview.byContent', { format: formatLabel(preview.choices.content) }),
                },
              ]}
            />
          </div>
        )}

        {/* Preview area: keyed by revision so a save shows the new version. */}
        <div key={revision} style={{ textAlign: 'center', marginBottom: fullscreen ? 0 : 16 }}>
          {loading || preview.pending ? (
            <Spin />
          ) : preview.error ? (
            <p style={{ fontSize: 12.5, color: 'var(--danger)', margin: 0 }}>
              Error: {preview.error}
            </p>
          ) : isImage && fileUrl ? (
            imageBlobType ? (
              <BlobImage
                url={fileUrl}
                type={imageBlobType}
                filename={file.name}
                style={imageStyle}
              />
            ) : (
              <Image src={fileUrl} alt={file.name} style={imageStyle} />
            )
          ) : isVideo && fileUrl ? (
            // oxlint-disable-next-line jsx-a11y/media-has-caption -- Object preview of the user's own file; no caption track exists.
            <video
              src={fileUrl}
              controls
              preload="metadata"
              poster={videoPoster ?? undefined}
              playsInline
              style={{
                display: 'block',
                margin: '0 auto',
                maxWidth: '100%',
                maxHeight: getPreviewMaxHeight(),
                borderRadius: 8,
              }}
            />
          ) : isAudio && fileUrl ? (
            // oxlint-disable-next-line jsx-a11y/media-has-caption -- Object preview of the user's own file; no caption track exists.
            <audio src={fileUrl} controls style={{ width: '100%' }} />
          ) : isText && fileUrl ? (
            <TextViewer
              url={fileUrl}
              filename={file.name}
              extension={target.extension}
              maxHeight={getPreviewMaxHeight()}
            />
          ) : kind === 'viewer' && fileUrl ? (
            <FileViewerPane
              url={fileUrl}
              filename={file.name}
              extension={target.extension}
              size={file.size}
              height={getPreviewMaxHeight()}
            />
          ) : (
            <span style={{ color: 'var(--accent)' }}>{placeholderIcon(target)}</span>
          )}
        </div>

        {/* URL display (fullscreen is for the content alone) */}
        {fullscreen ? null : fileUrl ? (
          <div
            style={{
              background: 'var(--bg-sunken)',
              padding: '10px 14px',
              borderRadius: 8,
              fontFamily: 'var(--font-mono)',
              fontSize: 11.5,
              color: 'var(--text-muted)',
              wordBreak: 'break-all',
              lineHeight: 1.5,
            }}
          >
            {fileUrl}
          </div>
        ) : needsCredentials ? (
          <p style={{ textAlign: 'center', fontSize: 12.5, color: 'var(--text-muted)', margin: 0 }}>
            {t('preview.needCreds')}
          </p>
        ) : (
          <p style={{ textAlign: 'center', fontSize: 12.5, color: 'var(--text-muted)', margin: 0 }}>
            {t('preview.noUrl')}
          </p>
        )}
      </Modal>
      {editing && config && editingAs && (
        <FileEditorModal
          key={file.key}
          config={config}
          file={file}
          editability={editingAs}
          previewContentType={target.contentType}
          onSaved={handleSaved}
          onClose={stopEditing}
        />
      )}
    </>
  );
}
