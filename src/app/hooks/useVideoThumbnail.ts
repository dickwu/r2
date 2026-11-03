'use client';

import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { VideoThumbnailCache } from '@/app/lib/videoThumbnailCache';

/** Width of the generated frame; grid tiles are ~160 px, at 2x plus margin. */
const THUMBNAIL_MAX_WIDTH = 480;

/** Error the native command returns when neither the bundled nor a system ffmpeg exists. */
export const FFMPEG_NOT_FOUND = 'ffmpeg_not_found';

const thumbnailCache = new VideoThumbnailCache();
/** Set by the last native attempt; lets a tile say why it has no preview. */
let ffmpegMissing = false;

export function isFfmpegMissing(): boolean {
  return ffmpegMissing;
}

const thumbnailPromiseCache = new Map<string, Promise<string | null>>();

interface UseVideoThumbnailOptions {
  lazy?: boolean;
  rootMargin?: string;
}

/**
 * JPEG data: URL of the frame at 1 s (the first frame for shorter videos),
 * rendered natively by ffmpeg. Resolves to null when ffmpeg is missing or
 * cannot read the video; callers then fall back to a `<video>` element.
 */
export async function getVideoThumbnailSrc(src: string): Promise<string | null> {
  const cached = thumbnailCache.get(src);
  if (cached !== undefined) {
    return cached;
  }

  const pending = thumbnailPromiseCache.get(src);
  if (pending) {
    return pending;
  }

  const request = invoke<string>('generate_video_thumbnail', {
    source: src,
    maxWidth: THUMBNAIL_MAX_WIDTH,
  })
    .then((thumbnail) => {
      ffmpegMissing = false;
      const value = thumbnail.startsWith('data:image/') ? thumbnail : null;
      thumbnailCache.set(src, value);
      return value;
    })
    .catch((error: unknown) => {
      ffmpegMissing = String(error) === FFMPEG_NOT_FOUND;
      if (ffmpegMissing) {
        console.warn('Video thumbnails: no ffmpeg found (bundled or system)');
      }
      thumbnailCache.set(src, null);
      return null;
    })
    .finally(() => {
      thumbnailPromiseCache.delete(src);
    });

  thumbnailPromiseCache.set(src, request);
  return request;
}

export function useVideoThumbnail(
  src: string | null,
  { lazy = false, rootMargin = '200px' }: UseVideoThumbnailOptions = {}
) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [thumbnailSrc, setThumbnailSrc] = useState<string | null>(null);
  const [shouldLoad, setShouldLoad] = useState(!lazy);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState(false);

  useEffect(() => {
    if (!src) {
      setThumbnailSrc(null);
      setError(false);
      setLoading(false);
      setShouldLoad(!lazy);
      return;
    }

    const cached = thumbnailCache.get(src);
    setThumbnailSrc(cached && cached.length > 0 ? cached : null);
    setError(cached === null);
    setLoading(false);
    setShouldLoad(!lazy || cached !== undefined);
  }, [lazy, src]);

  useEffect(() => {
    if (!lazy || shouldLoad || !src) {
      return;
    }

    const node = containerRef.current;
    if (!node) {
      return;
    }

    if (typeof window === 'undefined' || !('IntersectionObserver' in window)) {
      setShouldLoad(true);
      return;
    }

    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry?.isIntersecting) {
          setShouldLoad(true);
          observer.disconnect();
        }
      },
      { rootMargin }
    );

    observer.observe(node);

    return () => {
      observer.disconnect();
    };
  }, [lazy, rootMargin, shouldLoad, src]);

  useEffect(() => {
    if (!src || !shouldLoad) {
      return;
    }

    const cached = thumbnailCache.get(src);
    if (cached !== undefined) {
      setThumbnailSrc(cached && cached.length > 0 ? cached : null);
      setError(cached === null);
      setLoading(false);
      return;
    }

    let cancelled = false;
    setLoading(true);
    setError(false);

    void getVideoThumbnailSrc(src).then((thumbnail) => {
      if (cancelled) {
        return;
      }

      setThumbnailSrc(thumbnail);
      setError(thumbnail === null);
      setLoading(false);
    });

    return () => {
      cancelled = true;
    };
  }, [shouldLoad, src]);

  return {
    containerRef,
    error,
    loading,
    shouldLoad,
    thumbnailSrc,
  };
}
