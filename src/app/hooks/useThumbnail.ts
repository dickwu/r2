'use client';

import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { VideoThumbnailCache } from '@/app/lib/videoThumbnailCache';
import {
  memoryKeyIsObject,
  thumbnailMemoryKey,
  type ThumbnailKind,
  type ThumbnailObject,
} from '@/app/lib/thumbnailIdentity';

/** Width of the generated thumbnail; grid tiles are ~160 px, at 2x plus margin. */
export const THUMBNAIL_MAX_WIDTH = 480;

/** Error the native command returns when neither the bundled nor a system ffmpeg exists. */
export const FFMPEG_NOT_FOUND = 'ffmpeg_not_found';

interface ThumbnailResult {
  dataUrl: string;
  fromCache: boolean;
}

/** Session layer over the native disk cache: data: URLs (or null for a recent failure). */
const memory = new VideoThumbnailCache();
const pending = new Map<string, Promise<string | null>>();
/** Error of the last failed attempt per memory key, for callers that pick a fallback. */
const failures = new Map<string, string>();
let ffmpegMissing = false;

export function isFfmpegMissing(): boolean {
  return ffmpegMissing;
}

/** Why the last attempt for this thumbnail failed, if it did. */
export function thumbnailFailure(
  kind: ThumbnailKind,
  src: string,
  object?: ThumbnailObject | null
): string | undefined {
  return failures.get(thumbnailMemoryKey(kind, src, object));
}

/**
 * WebP data: URL of a small thumbnail: a downscaled image, or the frame at 1 s
 * (the first frame for shorter videos). With `object` the native side serves
 * it from the on-disk cache while the object is unchanged. Resolves to null
 * when it cannot be made; callers then fall back to the original.
 */
export async function getThumbnailSrc(
  kind: ThumbnailKind,
  src: string,
  object?: ThumbnailObject | null
): Promise<string | null> {
  const memoryKey = thumbnailMemoryKey(kind, src, object);
  const cached = memory.get(memoryKey);
  if (cached !== undefined) return cached;
  const inFlight = pending.get(memoryKey);
  if (inFlight) return inFlight;

  const request = invoke<ThumbnailResult>('get_thumbnail', {
    kind,
    source: src,
    maxWidth: THUMBNAIL_MAX_WIDTH,
    object: object ?? null,
  })
    .then((result) => {
      if (kind === 'video') ffmpegMissing = false;
      const value = result?.dataUrl?.startsWith('data:image/') ? result.dataUrl : null;
      failures.delete(memoryKey);
      memory.set(memoryKey, value);
      return value;
    })
    .catch((error: unknown) => {
      const message = String(error);
      if (kind === 'video') {
        ffmpegMissing = message === FFMPEG_NOT_FOUND;
        if (ffmpegMissing) console.warn('Video thumbnails: no ffmpeg found (bundled or system)');
      }
      failures.set(memoryKey, message);
      memory.set(memoryKey, null);
      return null;
    })
    .finally(() => {
      pending.delete(memoryKey);
    });

  pending.set(memoryKey, request);
  return request;
}

/** Forget every version of these objects (they changed or were deleted). */
export function forgetThumbnails(accountId: string, bucket: string, keys: string[]): number {
  if (keys.length === 0) return 0;
  const wanted = new Set(keys);
  const matches = (key: string) => memoryKeyIsObject(key, accountId, bucket, wanted);
  for (const key of failures.keys()) if (matches(key)) failures.delete(key);
  return memory.deleteWhere(matches);
}

/** Drop the session layer (the disk cache was cleared). */
export function clearThumbnailMemory(): void {
  memory.clear();
  failures.clear();
}

export function thumbnailMemorySize(): number {
  return memory.size;
}

interface UseThumbnailOptions {
  lazy?: boolean;
  rootMargin?: string;
  object?: ThumbnailObject | null;
}

export function useThumbnail(
  kind: ThumbnailKind,
  src: string | null,
  { lazy = false, rootMargin = '200px', object = null }: UseThumbnailOptions = {}
) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [thumbnailSrc, setThumbnailSrc] = useState<string | null>(null);
  const [shouldLoad, setShouldLoad] = useState(!lazy);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState(false);
  // Re-run on a new identity, not on a new object literal per render.
  const memoryKey = src ? thumbnailMemoryKey(kind, src, object) : null;
  const objectRef = useRef(object);
  objectRef.current = object;

  useEffect(() => {
    if (!memoryKey) {
      setThumbnailSrc(null);
      setError(false);
      setLoading(false);
      setShouldLoad(!lazy);
      return;
    }
    const cached = memory.get(memoryKey);
    setThumbnailSrc(cached && cached.length > 0 ? cached : null);
    setError(cached === null);
    setLoading(false);
    setShouldLoad(!lazy || cached !== undefined);
  }, [lazy, memoryKey]);

  useEffect(() => {
    if (!lazy || shouldLoad || !memoryKey) return;
    const node = containerRef.current;
    if (!node) return;
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
    return () => observer.disconnect();
  }, [lazy, rootMargin, shouldLoad, memoryKey]);

  useEffect(() => {
    if (!src || !memoryKey || !shouldLoad) return;
    const cached = memory.get(memoryKey);
    if (cached !== undefined) {
      setThumbnailSrc(cached && cached.length > 0 ? cached : null);
      setError(cached === null);
      setLoading(false);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(false);
    void getThumbnailSrc(kind, src, objectRef.current).then((thumbnail) => {
      if (cancelled) return;
      setThumbnailSrc(thumbnail);
      setError(thumbnail === null);
      setLoading(false);
    });
    return () => {
      cancelled = true;
    };
    // `src` changes with every re-signed URL; the identity is what matters.
  }, [kind, shouldLoad, memoryKey]);

  return { containerRef, error, loading, shouldLoad, thumbnailSrc };
}
