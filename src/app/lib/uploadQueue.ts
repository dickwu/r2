/**
 * Upload queue policy (issue #18). "Upload N file(s)" queues every selected
 * file and only this many transfer at once; the rest start as earlier ones
 * finish. The backend caps concurrent file uploads at 5 as a safety net, and
 * retries each request on transient failures (network, timeout, 408/429/5xx).
 */
export const DEFAULT_MAX_CONCURRENT_UPLOADS = 3;
export const MIN_CONCURRENT_UPLOADS = 3;
export const MAX_CONCURRENT_UPLOADS = 5;

/** A usable concurrency limit: an integer within [MIN, MAX]; default otherwise. */
export function clampUploadConcurrency(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_MAX_CONCURRENT_UPLOADS;
  return Math.min(MAX_CONCURRENT_UPLOADS, Math.max(MIN_CONCURRENT_UPLOADS, Math.round(value)));
}

interface QueueEntry {
  id: string;
  status: string;
}

/**
 * The queued tasks to start now, in queue order, so that no more than
 * `maxConcurrent` tasks are uploading.
 */
export function planUploadStarts<T extends QueueEntry>(tasks: T[], maxConcurrent: number): T[] {
  const limit = clampUploadConcurrency(maxConcurrent);
  const active = tasks.filter((t) => t.status === 'uploading').length;
  const free = limit - active;
  if (free <= 0) return [];
  return tasks.filter((t) => t.status === 'queued').slice(0, free);
}
