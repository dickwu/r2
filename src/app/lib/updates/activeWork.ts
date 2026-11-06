import { t as i18nT } from '@/i18n';
/**
 * Work a restart would cut off: transfers in flight, uploads a mounted drive
 * still has to send, and editor changes not saved yet. The status bar's
 * "N transfers active" uses the same transfer count.
 */

type Status = { status: string };

const UPLOAD_ACTIVE = new Set(['uploading', 'queued', 'pending']);
const DOWNLOAD_ACTIVE = new Set(['downloading', 'pending']);
const MOVE_ACTIVE = new Set(['downloading', 'uploading', 'finishing', 'deleting', 'pending']);

export const countActiveTransfers = (
  uploads: readonly Status[],
  downloads: readonly Status[],
  moves: readonly Status[]
): number =>
  uploads.filter((t) => UPLOAD_ACTIVE.has(t.status)).length +
  downloads.filter((t) => DOWNLOAD_ACTIVE.has(t.status)).length +
  moves.filter((t) => MOVE_ACTIVE.has(t.status)).length;

export interface ActiveWork {
  transfers: number;
  /** Files mounted drives have queued to upload. */
  mountUploads: number;
  /** Open editors with unsaved changes. */
  unsavedEditors: number;
}

export const hasActiveWork = (work: ActiveWork): boolean =>
  work.transfers > 0 || work.mountUploads > 0 || work.unsavedEditors > 0;

/**
 * What a restart right now would cost, as a sentence ending:
 * "stop 2 transfers and lose your unsaved changes in the editor". '' when nothing.
 */
export const describeActiveWork = (work: ActiveWork): string => {
  const parts: string[] = [];
  if (work.transfers > 0) {
    parts.push(i18nT('updates.work.transfers', { count: work.transfers }));
  }
  if (work.mountUploads > 0) {
    parts.push(i18nT('updates.work.mountUploads', { count: work.mountUploads }));
  }
  if (work.unsavedEditors > 0) parts.push(i18nT('updates.work.unsavedEditors'));
  if (parts.length <= 1) return parts[0] ?? '';
  return i18nT('common.listAnd', {
    head: parts.slice(0, -1).join(i18nT('common.listSeparator')),
    last: parts[parts.length - 1],
  });
};
