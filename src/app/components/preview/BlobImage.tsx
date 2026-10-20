'use client';

import { useEffect, useState, type CSSProperties } from 'react';
import { Image, Typography } from 'antd';
import DownloadProgress from '@/app/components/preview/DownloadProgress';
import {
  fetchPreviewBytes,
  PreviewFetchAbortedError,
  type PreviewDownloadProgress,
} from '@/app/components/preview/fetchPreviewBytes';

const { Text } = Typography;

interface BlobImageProps {
  url: string;
  /** MIME type to hand the bytes to the <img> as. */
  type: string;
  filename: string;
  style?: CSSProperties;
}

/**
 * An image shown from its downloaded bytes instead of its URL, for objects
 * stored under a type the webview would not decode it as (see
 * `getImageBlobType`).
 */
export default function BlobImage({ url, type, filename, style }: BlobImageProps) {
  const [src, setSrc] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [progress, setProgress] = useState<PreviewDownloadProgress>({ loaded: 0, total: null });

  useEffect(() => {
    setSrc(null);
    setError(null);
    setProgress({ loaded: 0, total: null });
    const controller = new AbortController();
    let objectUrl: string | null = null;

    fetchPreviewBytes(url, { signal: controller.signal, onProgress: setProgress })
      .then((bytes) => {
        if (controller.signal.aborted) return;
        objectUrl = URL.createObjectURL(new Blob([bytes as BlobPart], { type }));
        setSrc(objectUrl);
      })
      .catch((err: unknown) => {
        if (controller.signal.aborted || err instanceof PreviewFetchAbortedError) return;
        console.error('Failed to fetch preview:', err);
        setError(err instanceof Error ? err.message : 'Failed to load file');
      });

    return () => {
      controller.abort();
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [url, type]);

  if (error) {
    return (
      <div className="flex h-96 w-full items-center justify-center">
        <Text type="danger">Error: {error}</Text>
      </div>
    );
  }

  if (!src) {
    return <DownloadProgress loaded={progress.loaded} total={progress.total} filename={filename} />;
  }

  return <Image src={src} alt={filename} style={style} />;
}
