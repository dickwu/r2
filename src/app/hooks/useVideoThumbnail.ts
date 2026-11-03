'use client';

import type { ThumbnailObject } from '@/app/lib/thumbnailIdentity';
import { getThumbnailSrc, useThumbnail } from '@/app/hooks/useThumbnail';

export { FFMPEG_NOT_FOUND, isFfmpegMissing } from '@/app/hooks/useThumbnail';

interface UseVideoThumbnailOptions {
  lazy?: boolean;
  rootMargin?: string;
  object?: ThumbnailObject | null;
}

/**
 * WebP data: URL of the frame at 1 s (the first frame for shorter videos),
 * rendered natively by ffmpeg and kept in the on-disk thumbnail cache when
 * `object` identifies it. Resolves to null when ffmpeg is missing or cannot
 * read the video; callers then fall back to a `<video>` element.
 */
export function getVideoThumbnailSrc(
  src: string,
  object?: ThumbnailObject | null
): Promise<string | null> {
  return getThumbnailSrc('video', src, object);
}

export function useVideoThumbnail(src: string | null, options: UseVideoThumbnailOptions = {}) {
  return useThumbnail('video', src, options);
}
