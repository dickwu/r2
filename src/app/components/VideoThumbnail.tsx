'use client';

import { useEffect, useState } from 'react';
import { PlaySquareOutlined, LoadingOutlined } from '@ant-design/icons';
import { generateThumbnail } from 'tauri-plugin-video-thumbnail';

interface VideoThumbnailProps {
  src: string;
  alt?: string;
}

export default function VideoThumbnail({ src, alt }: VideoThumbnailProps) {
  const [thumbnailSrc, setThumbnailSrc] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(false);

  useEffect(() => {
    let cancelled = false;

    const loadThumbnail = async () => {
      try {
        const result = await generateThumbnail({
          source: src,
          size: 'large',
        });

        if (!cancelled && result.base64) {
          setThumbnailSrc(`data:image/png;base64,${result.base64}`);
          setLoading(false);
        }
      } catch (e) {
        console.error('Failed to generate thumbnail:', e);
        if (!cancelled) {
          setError(true);
          setLoading(false);
        }
      }
    };

    loadThumbnail();

    return () => {
      cancelled = true;
    };
  }, [src]);

  if (error) {
    return (
      <div className="video-thumbnail-fallback">
        <PlaySquareOutlined className="video-icon" />
      </div>
    );
  }

  return (
    <div className="video-thumbnail">
      {thumbnailSrc && (
        <img src={thumbnailSrc} alt={alt} style={{ display: loading ? 'none' : 'block' }} />
      )}
      {loading && (
        <div className="video-thumbnail-loading">
          <LoadingOutlined />
        </div>
      )}
      <PlaySquareOutlined className="play-icon" />
    </div>
  );
}
