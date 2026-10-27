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

const plural = (n: number, one: string, many: string) => (n === 1 ? one : many);

/**
 * What a restart right now would cost, as a sentence ending:
 * "stop 2 transfers and lose your unsaved changes in the editor". '' when nothing.
 */
export const describeActiveWork = (work: ActiveWork): string => {
  const parts: string[] = [];
  if (work.transfers > 0) {
    parts.push(`stop ${work.transfers} ${plural(work.transfers, 'transfer', 'transfers')}`);
  }
  if (work.mountUploads > 0) {
    parts.push(
      `stop ${work.mountUploads} ${plural(work.mountUploads, 'upload', 'uploads')} from a mounted drive`
    );
  }
  if (work.unsavedEditors > 0) parts.push('lose your unsaved changes in the editor');
  if (parts.length <= 1) return parts[0] ?? '';
  return `${parts.slice(0, -1).join(', ')} and ${parts[parts.length - 1]}`;
};
