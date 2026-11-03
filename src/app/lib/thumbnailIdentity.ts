import { thumbnailCacheKey } from '@/app/lib/videoThumbnailCache';

export type ThumbnailKind = 'image' | 'video';

/**
 * The object a thumbnail is of. The native cache keys its files and index rows
 * on this (never on a presigned URL): the same identity is served from disk,
 * a changed size / Last-Modified / ETag makes it regenerate.
 */
export interface ThumbnailObject {
  accountId: string;
  bucket: string;
  key: string;
  etag?: string;
  size?: number;
  lastModified?: string;
}

/** Payload of the native `thumbnail-cache-invalidated` event. */
export interface ThumbnailInvalidation {
  accountId: string;
  bucket: string;
  changed: { key: string; kind: ThumbnailKind; size?: number; lastModified?: string }[];
  removed: string[];
}

export const THUMBNAIL_INVALIDATED_EVENT = 'thumbnail-cache-invalidated';
export const THUMBNAIL_CLEARED_EVENT = 'thumbnail-cache-cleared';

/** Error the native command returns for images above its download cap. */
export const IMAGE_TOO_LARGE = 'image_too_large';

/**
 * In-memory cache key. With an object it is the object's identity, so a
 * re-signed URL reuses the entry and a changed object misses it. Without one it
 * is the URL minus its query. JSON keeps fields from running into each other
 * and never parses as a URL.
 */
export function thumbnailMemoryKey(
  kind: ThumbnailKind,
  src: string,
  object?: ThumbnailObject | null
): string {
  if (!object) return JSON.stringify([kind, 'url', thumbnailCacheKey(src)]);
  return JSON.stringify([
    kind,
    'object',
    object.accountId,
    object.bucket,
    object.key,
    object.etag ?? null,
    object.size ?? null,
    object.lastModified ?? null,
  ]);
}

/** Whether a memory key belongs to `key` in that account and bucket, any version. */
export function memoryKeyIsObject(
  memoryKey: string,
  accountId: string,
  bucket: string,
  keys: ReadonlySet<string>
): boolean {
  try {
    const parts = JSON.parse(memoryKey) as unknown[];
    return (
      parts[1] === 'object' &&
      parts[2] === accountId &&
      parts[3] === bucket &&
      keys.has(parts[4] as string)
    );
  } catch {
    return false;
  }
}

/** Most changed thumbnails remade ahead of time per event; the rest wait for a tile. */
export const MAX_PROACTIVE_REFRESH = 48;

export interface ThumbnailRefreshPlan {
  /** Keys whose remembered thumbnails are out of date. */
  forget: string[];
  /** Changed objects to remake now, so their tiles find the new version on disk. */
  regenerate: (ThumbnailObject & { kind: ThumbnailKind })[];
}

/** What an invalidation means for the bucket on screen; nothing for another one. */
export function planThumbnailRefresh(
  event: ThumbnailInvalidation,
  current: { accountId: string; bucket: string } | null | undefined
): ThumbnailRefreshPlan {
  if (!current || event.accountId !== current.accountId || event.bucket !== current.bucket) {
    return { forget: [], regenerate: [] };
  }
  const changed = event.changed ?? [];
  const removed = event.removed ?? [];
  return {
    forget: [...changed.map((item) => item.key), ...removed],
    regenerate: changed
      .filter((item) => item.kind === 'image' || item.kind === 'video')
      .slice(0, MAX_PROACTIVE_REFRESH)
      .map((item) => ({
        kind: item.kind,
        accountId: event.accountId,
        bucket: event.bucket,
        key: item.key,
        size: item.size ?? undefined,
        lastModified: item.lastModified ?? undefined,
      })),
  };
}
