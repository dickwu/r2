import { invoke } from '@tauri-apps/api/core';

/** What the native thumbnail cache holds, and its limits. */
export interface ThumbnailCacheStats {
  count: number;
  bytes: number;
  maxBytes: number;
  /** Images larger than this keep loading the original instead of a thumbnail. */
  imageMaxBytes: number;
}

const MB = 1024 * 1024;

export const THUMBNAIL_LIMIT_OPTIONS = [
  { bytes: 250 * MB, label: '250 MB', sub: 'Light: about 10,000 photos' },
  { bytes: 500 * MB, label: '500 MB', sub: 'Default: about 20,000 photos' },
  { bytes: 1024 * MB, label: '1 GB', sub: 'Large libraries' },
  { bytes: 2048 * MB, label: '2 GB', sub: 'Very large libraries' },
] as const;

/** Share of the limit in use, 0–100, for the meter. */
export function cacheUsagePercent(stats: Pick<ThumbnailCacheStats, 'bytes' | 'maxBytes'>): number {
  if (!stats.maxBytes || stats.maxBytes <= 0) return 0;
  return Math.min(100, Math.max(0, (stats.bytes / stats.maxBytes) * 100));
}

export function getThumbnailCacheStats(): Promise<ThumbnailCacheStats> {
  return invoke<ThumbnailCacheStats>('get_thumbnail_cache_stats');
}

export function setThumbnailCacheLimit(maxBytes: number): Promise<ThumbnailCacheStats> {
  return invoke<ThumbnailCacheStats>('set_thumbnail_cache_limit', { maxBytes });
}

export function clearThumbnailCache(): Promise<ThumbnailCacheStats> {
  return invoke<ThumbnailCacheStats>('clear_thumbnail_cache');
}
