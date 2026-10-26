'use client';

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
import {
  getFileExtension,
  getImageBlobType,
  type PreviewTarget,
} from '@/app/components/preview/previewKind';
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
  image: 'Image',
  video: 'Video',
  audio: 'Audio',
  text: 'Text',
  viewer: 'File',
};

/** Short name of the format a preview renders, e.g. "PDF". */
function formatLabel({ kind, extension }: PreviewTarget): string {
  return extension ? extension.toUpperCase() : KIND_LABELS[kind];
}

interface FilePreviewModalProps {
  config?: StorageConfig | null;
  onCredentialsUpdate?: () => void;
  onFileUpdated?: () => void;
}

export default function FilePreviewModal({ config, onFileUpdated }: FilePreviewModalProps) {
  const { message } = App.useApp();
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

  useEffect(() => {
    if (!isOpen || !file) {
      setObjectUrl(null);
      return;
    }
    const { key } = file;

    if (isPublic && config) {
      // Public bucket: reuse the provider's URL builder so the object key is
      // encoded consistently (spaces, unicode, reserved characters) — no signing.
      setObjectUrl({ key, url: buildPublicUrl(config, key) });
      return;
    }

    if (config && hasCredentials) {
      // Private bucket: auto-sign the URL for the preview. A signature that
      // lands after the user moved on is dropped.
      let stale = false;
      setLoading(true);
      generateSignedUrl(config, key)
        .then((url) => {
          if (!stale) setObjectUrl({ key, url });
        })
        .catch(() => {
          if (!stale) setObjectUrl({ key, url: null });
        })
        .finally(() => {
          if (!stale) setLoading(false);
        });
      return () => {
        stale = true;
        setLoading(false);
      };
    }

    setObjectUrl(null);
  }, [isOpen, file, config, hasCredentials, isPublic]);

  const { thumbnailSrc: videoPoster } = useVideoThumbnail(
    file && preview.target.kind === 'video' ? signedUrl : null
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
      message.warning('No public domain configured');
      return;
    }
    try {
      await navigator.clipboard.writeText(fileUrl);
      message.success('URL copied to clipboard');
    } catch {
      message.error('Failed to copy URL');
    }
  }

  async function handleOpenUrl() {
    if (!fileUrl) {
      message.warning('No public domain configured');
      return;
    }
    try {
      await openUrl(fileUrl);
    } catch {
      message.error('Failed to open URL');
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
          <LinkOutlined /> Open URL
        </button>
        <button className="btn" onClick={handleCopyUrl} disabled={!fileUrl}>
          <CopyOutlined /> Copy URL
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
                ? 'Editing needs access keys for this bucket. Add them in Account Settings.'
                : 'Choose a bucket to edit its files.'
              : editability.editable
                ? 'Edit this file'
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
            <EditOutlined /> Edit
          </button>
        </Tooltip>
      )}

      <button className="btn btn-primary" onClick={close}>
        Close
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
              The content is {preview.choices.contentName}, but the name says{' '}
              {formatLabel(preview.choices.name)}. Render as
            </span>
            <Segmented<PreviewSource>
              size="small"
              value={preview.source}
              onChange={preview.setSource}
              options={[
                { value: 'name', label: `${formatLabel(preview.choices.name)} · name` },
                { value: 'content', label: `${formatLabel(preview.choices.content)} · content` },
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
            S3 credentials required for signed URL. Configure in Account Settings.
          </p>
        ) : (
          <p style={{ textAlign: 'center', fontSize: 12.5, color: 'var(--text-muted)', margin: 0 }}>
            Unable to generate file URL
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
