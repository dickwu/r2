'use client';

import { useEffect, useRef, useState } from 'react';
import { generateThumbnail } from 'tauri-plugin-video-thumbnail';

const thumbnailCache = new Map<string, string | null>();
const thumbnailPromiseCache = new Map<string, Promise<string | null>>();

interface UseVideoThumbnailOptions {
  lazy?: boolean;
  rootMargin?: string;
}

export async function getVideoThumbnailSrc(src: string): Promise<string | null> {
  const cached = thumbnailCache.get(src);
  if (cached !== undefined) {
    return cached;
  }

  const pending = thumbnailPromiseCache.get(src);
  if (pending) {
    return pending;
  }

  const request = generateThumbnail({
    source: src,
    size: 'large',
  })
    .then((result) => {
      const thumbnail = result.base64 ? `data:image/png;base64,${result.base64}` : null;
      thumbnailCache.set(src, thumbnail);
      return thumbnail;
    })
    .catch(() => {
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
