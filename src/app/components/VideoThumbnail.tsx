'use client';

import { useEffect, useState } from 'react';
import { PlaySquareOutlined, LoadingOutlined } from '@ant-design/icons';
import { isFfmpegMissing, useVideoThumbnail } from '@/app/hooks/useVideoThumbnail';
import { mediaFragmentUrl } from '@/app/lib/videoThumbnailCache';

interface VideoThumbnailProps {
  src: string;
  alt?: string;
}

/**
 * Grid thumbnail for a video: the frame at 1 s rendered by the bundled ffmpeg.
 * When ffmpeg cannot read the file (or is missing) the webview's own player
 * shows the same frame through a `#t=1` media fragment, fetching only the
 * metadata and the bytes around that frame. A video neither can decode gets
 * the play icon, labelled "ffmpeg not found" when that is the reason.
 */
export default function VideoThumbnail({ src, alt }: VideoThumbnailProps) {
  const { containerRef, error, loading, shouldLoad, thumbnailSrc } = useVideoThumbnail(src, {
    lazy: true,
  });
  const [playerFailed, setPlayerFailed] = useState(false);
  const [playerReady, setPlayerReady] = useState(false);

  useEffect(() => {
    setPlayerFailed(false);
    setPlayerReady(false);
  }, [src]);

  if (error && playerFailed) {
    const reason = isFfmpegMissing() ? 'ffmpeg not found' : null;
    return (
      <div
        ref={containerRef}
        className="video-thumbnail-fallback"
        title={reason ? 'No preview: ffmpeg not found' : 'No preview available'}
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
