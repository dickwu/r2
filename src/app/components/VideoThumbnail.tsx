'use client';

import { PlaySquareOutlined, LoadingOutlined } from '@ant-design/icons';
import { useVideoThumbnail } from '@/app/hooks/useVideoThumbnail';

interface VideoThumbnailProps {
  src: string;
  alt?: string;
}

export default function VideoThumbnail({ src, alt }: VideoThumbnailProps) {
  const { containerRef, error, loading, shouldLoad, thumbnailSrc } = useVideoThumbnail(src, {
    lazy: true,
  });

  if (error) {
    return (
      <div ref={containerRef} className="video-thumbnail-fallback">
        <PlaySquareOutlined className="video-icon" />
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
