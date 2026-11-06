import { t as i18nT } from '@/i18n';

/**
 * Format bytes to human-readable string (e.g., "1.5 MB")
 */
export function formatBytes(bytes: number): string {
  if (bytes === 0) return '0 B';
  const k = 1024;
  const sizes = ['B', 'KB', 'MB', 'GB', 'TB'];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + ' ' + sizes[i];
}

/**
 * Format bytes/second to human-readable speed (e.g., "12.5 MB/s")
 */
export function formatSpeed(bytesPerSecond: number): string {
  if (bytesPerSecond === 0) return '0 B/s';
  const k = 1024;
  const sizes = ['B/s', 'KB/s', 'MB/s', 'GB/s'];
  const i = Math.floor(Math.log(bytesPerSecond) / Math.log(k));
  return parseFloat((bytesPerSecond / Math.pow(k, i)).toFixed(1)) + ' ' + sizes[i];
}

/**
 * Format seconds to human-readable time remaining (e.g., "2m 30s left")
 */
const left = (duration: string): string => i18nT('time.left', { duration });

export function formatTimeLeft(seconds: number): string {
  if (seconds <= 0) return '';
  if (seconds < 60) return left(`${Math.ceil(seconds)}s`);
  if (seconds < 3600) {
    const mins = Math.floor(seconds / 60);
    const secs = Math.ceil(seconds % 60);
    return left(secs > 0 ? `${mins}m ${secs}s` : `${mins}m`);
  }
  const hours = Math.floor(seconds / 3600);
  const mins = Math.floor((seconds % 3600) / 60);
  return left(mins > 0 ? `${hours}h ${mins}m` : `${hours}h`);
}

/**
 * Format seconds to compact ETA (e.g., "~4m left")
 */
export function formatEta(seconds: number): string {
  if (seconds <= 0 || !isFinite(seconds)) return '';
  if (seconds < 60) return left(`~${Math.ceil(seconds)}s`);
  if (seconds < 3600) return left(`~${Math.floor(seconds / 60)}m`);
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  return left(m > 0 ? `~${h}h ${m}m` : `~${h}h`);
}
