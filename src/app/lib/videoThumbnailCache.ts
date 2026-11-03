/**
 * Session cache for generated video thumbnails.
 *
 * Entries are keyed by the object's URL without its query string, so a
 * presigned URL that is re-signed (a new signature every 45 minutes) reuses
 * the frame already rendered for that object. A failure is only remembered
 * for {@link THUMBNAIL_FAILURE_RETRY_MS}: a network blip, or ffmpeg being
 * installed while the app runs, does not leave a tile without a thumbnail for
 * the rest of the session.
 */

/** Seconds into the video the thumbnail frame is taken from. */
export const THUMBNAIL_SEEK_SECONDS = 1;
export const THUMBNAIL_FAILURE_RETRY_MS = 60_000;

/** Stable cache key for a video source: http(s) URLs lose their query and hash. */
export function thumbnailCacheKey(src: string): string {
  try {
    const url = new URL(src);
    if (url.protocol === 'http:' || url.protocol === 'https:') {
      return `${url.origin}${url.pathname}`;
    }
  } catch {
    // Not a URL (a local path): use it as is.
  }
  return src;
}

/**
 * `src` with a media fragment that makes a `<video>` element show the frame
 * at `seconds` (browsers clamp it to the duration for shorter videos).
 */
export function mediaFragmentUrl(src: string, seconds = THUMBNAIL_SEEK_SECONDS): string {
  const hash = src.indexOf('#');
  const base = hash === -1 ? src : src.slice(0, hash);
  return `${base}#t=${seconds}`;
}

interface Entry {
  /** data: URL of the frame, or null when generation failed. */
  value: string | null;
  at: number;
}

export class VideoThumbnailCache {
  private entries = new Map<string, Entry>();

  constructor(private readonly failureRetryMs = THUMBNAIL_FAILURE_RETRY_MS) {}

  /**
   * The cached frame, `null` for a recent failure, or `undefined` when the
   * thumbnail should be (re)generated.
   */
  get(src: string, now = Date.now()): string | null | undefined {
    const key = thumbnailCacheKey(src);
    const entry = this.entries.get(key);
    if (!entry) return undefined;
    if (entry.value === null && now - entry.at >= this.failureRetryMs) {
      this.entries.delete(key);
      return undefined;
    }
    return entry.value;
  }

  set(src: string, value: string | null, now = Date.now()): void {
    this.entries.set(thumbnailCacheKey(src), { value, at: now });
  }

  /** Drop the entries whose key passes `matches`; returns how many went. */
  deleteWhere(matches: (key: string) => boolean): number {
    let removed = 0;
    for (const key of [...this.entries.keys()]) {
      if (matches(key)) {
        this.entries.delete(key);
        removed += 1;
      }
    }
    return removed;
  }

  clear(): void {
    this.entries.clear();
  }

  get size(): number {
    return this.entries.size;
  }
}
