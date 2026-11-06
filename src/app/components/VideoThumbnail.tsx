'use client';

import { useTranslation } from 'react-i18next';
import { useEffect, useState } from 'react';
import { PlaySquareOutlined, LoadingOutlined } from '@ant-design/icons';
import { isFfmpegMissing, useVideoThumbnail } from '@/app/hooks/useVideoThumbnail';
import { mediaFragmentUrl } from '@/app/lib/videoThumbnailCache';
import type { ThumbnailObject } from '@/app/lib/thumbnailIdentity';

interface VideoThumbnailProps {
  src: string;
  alt?: string;
  /** The object, so the frame is kept in the on-disk thumbnail cache. */
  object?: ThumbnailObject | null;
}

/**
 * Grid thumbnail for a video: the frame at 1 s rendered by the bundled ffmpeg
 * (served from the on-disk cache while the object is unchanged).
 * When ffmpeg cannot read the file (or is missing) the webview's own player
 * shows the same frame through a `#t=1` media fragment, fetching only the
 * metadata and the bytes around that frame. A video neither can decode gets
 * the play icon, labelled "ffmpeg not found" when that is the reason.
 */
export default function VideoThumbnail({ src, alt, object }: VideoThumbnailProps) {
  const { containerRef, error, loading, shouldLoad, thumbnailSrc } = useVideoThumbnail(src, {
    lazy: true,
    object,
  });
  const [playerFailed, setPlayerFailed] = useState(false);
  const [playerReady, setPlayerReady] = useState(false);
  const { t } = useTranslation();

  useEffect(() => {
    setPlayerFailed(false);
    setPlayerReady(false);
  }, [src]);

  if (error && playerFailed) {
    const reason = isFfmpegMissing() ? t('preview.ffmpegMissing') : null;
    return (
      <div
        ref={containerRef}
        className="video-thumbnail-fallback"
        title={reason ? t('preview.noPreviewReason', { reason }) : t('preview.noPreview')}
      >
        <PlaySquareOutlined className="video-icon" />
        {reason && <span className="video-thumbnail-note">{reason}</span>}
      </div>
    );
  }

  if (error) {
    return (
      <div ref={containerRef} className="video-thumbnail">
        <video
          src={mediaFragmentUrl(src)}
          aria-label={alt}
          preload="metadata"
          muted
          playsInline
          disablePictureInPicture
          tabIndex={-1}
          onLoadedData={() => setPlayerReady(true)}
          onError={() => setPlayerFailed(true)}
          style={{ visibility: playerReady ? 'visible' : 'hidden' }}
        />
        {!playerReady && (
          <div className="video-thumbnail-loading">
            <LoadingOutlined />
          </div>
        )}
        <PlaySquareOutlined className="play-icon" />
      </div>
    );
  }

  return (
    <div ref={containerRef} className="video-thumbnail">
      {thumbnailSrc && (
        <img
          src={thumbnailSrc}
          alt={alt}
          loading="lazy"
          style={{ display: loading ? 'none' : 'block' }}
        />
      )}
      {shouldLoad && loading && (
        <div className="video-thumbnail-loading">
          <LoadingOutlined />
        </div>
      )}
      <PlaySquareOutlined className="play-icon" />
    </div>
  );
}
