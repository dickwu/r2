/**
 * A transfer that stopped, as the failure modal shows it. Stores report these
 * when a task enters a failed state, rows and the dock show them again on
 * request, and the modal renders them — so the shape and its wording live
 * here, pure: no React, no Tauri.
 */

export type TransferFailureKind =
  'upload' | 'download' | 'move' | 'rename' | 'sync' | 'delete' | 'mount';

export interface TransferFailureProgress {
  /** Bytes (or items for delete/rename) completed before the failure. */
  done: number;
  /** Bytes/items expected; when <= 0 the fault line is not drawn. */
  total: number;
}

export interface TransferFailure {
  /** Namespaced, e.g. `upload:${task.id}`, `sync:${accountId}/${bucket}`. */
  id: string;
  kind: TransferFailureKind;
  /** What the person recognises: file name, "3 objects", bucket name. */
  name: string;
  /** The error text verbatim (may be multi-line). */
  message: string;
  /** Epoch ms, `Date.now()` when the failure was seen. */
  occurredAt: number;
  /** Object key / path when there is one. */
  key?: string;
  bucket?: string;
  /** Account display name. */
  account?: string;
  /** e.g. "Waiting for credentials" (move recovery label), "Part 7 of 11". */
  stage?: string;
  progress?: TransferFailureProgress | null;
  /** What the header says for this record alone; `${KIND_LABEL[kind]} failed` when unset. */
  title?: string;
}

export const KIND_LABEL: Record<TransferFailureKind, string> = {
  upload: 'Upload',
  download: 'Download',
  move: 'Move',
  rename: 'Rename',
  sync: 'Sync',
  delete: 'Delete',
  mount: 'Mount transfer',
};

/** Kinds whose progress counts items (objects deleted, files renamed) rather than bytes. */
export const isCountKind = (kind: TransferFailureKind): boolean =>
  kind === 'delete' || kind === 'rename';

/** Plain item count for the delete/rename progress formatter, e.g. "1,200". */
export const formatCount = (n: number): string => Math.round(n).toLocaleString('en-US');

// A provider error code (`AccessDenied`, `NoSuchKey`) is one PascalCase token
// directly followed by ": " — the shape describe_s3_error writes. A code with
// nothing after it matches too, so it can come back as the text instead.
const CODE_PREFIX = /^([A-Z][A-Za-z0-9]{2,}):(?: |$)/;

/**
 * "AccessDenied: Access Denied" → { code: 'AccessDenied', text: 'Access Denied' }.
 * "Upload failed: 403 - …", "transient: GET: try again", "no route to host; …"
 * → { code: null, text: message }. Text is trimmed; a message that is only a
 * code keeps the code as text, so the modal never shows an empty message.
 */
export const splitErrorCode = (message: string): { code: string | null; text: string } => {
  const trimmed = message.trim();
  const match = CODE_PREFIX.exec(trimmed);
  if (!match) return { code: null, text: trimmed };
  const text = trimmed.slice(match[0].length).trim();
  return text === '' ? { code: null, text: match[1] } : { code: match[1], text };
};

/**
 * Whole-number percent 0..100, clamped, or null when total <= 0 / progress is
 * missing or not a number. Rounds like the dock's lane bar, except that an
 * unfinished transfer never reads 100: "stopped at 100%" would claim a finish.
 */
export const failurePercent = (progress?: TransferFailureProgress | null): number | null => {
  if (!progress) return null;
  const { done, total } = progress;
  if (!(total > 0) || !Number.isFinite(total) || !Number.isFinite(done)) return null;
  const pct = Math.min(100, Math.max(0, Math.round((done / total) * 100)));
  return pct === 100 && done < total ? 99 : pct;
};

/** "41.2 MB of 66.5 MB", or "3 of 120 items" for delete/rename; null when there is no progress to show. */
export const formatFailureAmount = (
  failure: Pick<TransferFailure, 'kind' | 'progress'>,
  format: (n: number) => string
): string | null => {
  const { progress } = failure;
  if (!progress || failurePercent(progress) === null) return null;
  // Whole units: progress derived from a percent can be fractional, and
  // formatBytes has no unit for values below one byte.
  const total = Math.round(progress.total);
  const done = Math.round(Math.min(Math.max(progress.done, 0), progress.total));
  const amount = `${format(done)} of ${format(total)}`;
  if (!isCountKind(failure.kind)) return amount;
  return `${amount} ${total === 1 ? 'item' : 'items'}`;
};

/** What one record's header says: its own title, else "Upload failed". */
export const singleFailureTitle = (failure: Pick<TransferFailure, 'kind' | 'title'>): string =>
  failure.title ?? `${KIND_LABEL[failure.kind]} failed`;

/** Title for one failure: "Upload failed". For several: "3 failures". */
export const failureTitle = (failures: readonly TransferFailure[]): string =>
  failures.length === 1 ? singleFailureTitle(failures[0]) : `${failures.length} failures`;

/** The first non-empty line of a message — what a one-line list row has room for. */
export const firstLine = (message: string): string =>
  message
    .split('\n')
    .find((line) => line.trim() !== '')
    ?.trim() ?? '';

const usableTime = (at: number): boolean => Number.isFinite(at) && at > 0;

const pad2 = (n: number): string => String(n).padStart(2, '0');

/** Local `HH:MM:SS`, or '' when the time is unusable. */
export const formatFailureClock = (at: number): string => {
  if (!usableTime(at)) return '';
  const d = new Date(at);
  return `${pad2(d.getHours())}:${pad2(d.getMinutes())}:${pad2(d.getSeconds())}`;
};

/** Local `YYYY-MM-DD HH:mm:ss`, or '' when the time is unusable. */
export const formatFailureTimestamp = (at: number): string => {
  if (!usableTime(at)) return '';
  const d = new Date(at);
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())} ${formatFailureClock(at)}`;
};

const hasText = (value: string | null | undefined): value is string =>
  typeof value === 'string' && value.trim() !== '';

/**
 * Plain text for the clipboard and the report description:
 *
 *   Upload failed — IMG_2214.CR2
 *   Account: Greenwoods R2
 *   Bucket: media
 *   Key: photos/2026/IMG_2214.CR2
 *   Stage: Part 7 of 11
 *   Progress: 41.2 MB of 66.5 MB (62%)
 *   When: 2026-09-25 10:42:07
 *
 *   AccessDenied: The AWS Access Key Id you provided does not exist in our records.
 *
 * Lines without a value are left out. `formatBytes` formats the progress
 * numbers — the caller passes a plain-count formatter for delete/rename, whose
 * line then reads "3 of 120 items (2%)". `now` stands in for a record without a
 * usable `occurredAt`, so the When line is never blank.
 */
export const formatFailureDetails = (
  failure: TransferFailure,
  formatBytes: (n: number) => string,
  now: number = Date.now()
): string => {
  const failed = singleFailureTitle(failure);
  const heading = hasText(failure.name) ? `${failed} — ${failure.name}` : failed;
  const amount = formatFailureAmount(failure, formatBytes);
  const when = formatFailureTimestamp(failure.occurredAt) || formatFailureTimestamp(now);
  const facts: ReadonlyArray<readonly [string, string | null | undefined]> = [
    ['Account', failure.account],
    ['Bucket', failure.bucket],
    ['Key', failure.key],
    ['Stage', failure.stage],
    ['Progress', amount === null ? null : `${amount} (${failurePercent(failure.progress)}%)`],
    ['When', when],
  ];
  const lines = [
    heading,
    ...facts.flatMap(([label, value]) => (hasText(value) ? [`${label}: ${value}`] : [])),
  ];
  const message = failure.message.trim();
  return (message === '' ? lines : [...lines, '', message]).join('\n');
};
