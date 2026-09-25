/**
 * Failure records for the task rows: uploads, downloads and moves. A store
 * reports one when its task enters a failed state, and the row's "Show error"
 * button shows it again. Both build it here, from the task, so the failure
 * modal says the same thing whichever surface opened it. Pure: no React, no
 * stores; the store imports are types only.
 */

import type { StorageProvider } from '@/app/lib/r2cache';
import type { TransferFailure } from '@/app/lib/transferFailure';
import type { ProviderAccount } from '@/app/stores/accountStore';
import type { DownloadTask } from '@/app/stores/downloadStore';
import type { MoveTask } from '@/app/stores/moveStore';
import type { UploadTask } from '@/app/stores/uploadStore';

export interface TaskFailureContext {
  /** Epoch ms the failure was seen; defaults to now. */
  occurredAt?: number;
}

/** Where an upload was going. The upload task does not carry it; the upload store's destination does. */
export interface UploadFailureContext extends TaskFailureContext {
  /** The object key the upload wrote to. */
  key?: string;
  bucket?: string;
  /** Account display name. */
  account?: string;
}

export interface MoveFailureContext extends TaskFailureContext {
  /** The recovery label of a move that waits on the person instead of having failed outright. */
  stage?: string;
}

/** Bytes done out of `fileSize` at `percent`, for tasks that keep only a percent. */
const bytesAt = (percent: number, fileSize: number): number =>
  Math.round((percent / 100) * fileSize);

/** "photos/2026/IMG_2214.CR2" → "IMG_2214.CR2", the name the dock gives a move. */
const baseName = (key: string): string => key.split('/').filter(Boolean).pop() ?? key;

/**
 * A move restored from the database knows its percent but not its bytes
 * (`transferredBytes` stays 0 until a progress event arrives), so take
 * whichever says more.
 */
const movedBytes = (task: MoveTask): number =>
  Math.max(task.transferredBytes, bytesAt(task.progress, task.fileSize));

export const uploadFailure = (
  task: UploadTask,
  ctx: UploadFailureContext = {}
): TransferFailure => ({
  id: `upload:${task.id}`,
  kind: 'upload',
  name: task.fileName,
  message: task.error || 'Upload failed',
  occurredAt: ctx.occurredAt ?? Date.now(),
  key: ctx.key,
  bucket: ctx.bucket,
  account: ctx.account,
  progress: { done: bytesAt(task.progress, task.fileSize), total: task.fileSize },
});

export const downloadFailure = (
  task: DownloadTask,
  ctx: TaskFailureContext = {}
): TransferFailure => ({
  id: `download:${task.id}`,
  kind: 'download',
  name: task.fileName,
  message: task.error || 'Download failed',
  occurredAt: ctx.occurredAt ?? Date.now(),
  key: task.key,
  bucket: task.bucket,
  progress: { done: task.downloadedBytes, total: task.fileSize },
});

/**
 * A move that failed outright, or — given the recovery label as `stage` — one
 * that waits on the person. The two are separate records: a recovery note
 * shown from the row must not read as the same event as a later failure with
 * the same text, which would otherwise not open the modal.
 */
export const moveFailure = (task: MoveTask, ctx: MoveFailureContext = {}): TransferFailure => ({
  id: ctx.stage ? `move-recovery:${task.id}` : `move:${task.id}`,
  kind: 'move',
  name: baseName(task.sourceKey),
  message: task.error || 'Move failed',
  occurredAt: ctx.occurredAt ?? Date.now(),
  key: `${task.sourceKey} → ${task.destKey}`,
  bucket: task.destBucket,
  stage: ctx.stage,
  progress: { done: movedBytes(task), total: task.fileSize },
  // The status bar's own words for these moves.
  title: ctx.stage ? 'Move needs attention' : undefined,
});

/**
 * What "Show error" should open: the record reported when the task failed,
 * while it still carries the same message, else `fresh`. The reported record
 * holds the context of that moment, which for an upload is not on the task:
 * the destination folder can change while the upload modal stays open.
 */
export const preferRecorded = (
  fresh: TransferFailure,
  recorded: readonly TransferFailure[]
): TransferFailure =>
  recorded.find((f) => f.id === fresh.id && f.message === fresh.message) ?? fresh;

/** An account as the app labels it everywhere: its name, else its id. */
export const accountDisplayName = (
  accounts: readonly ProviderAccount[],
  provider: StorageProvider,
  accountId: string
): string => {
  const match = accounts.find((a) => a.provider === provider && a.account.id === accountId);
  return match?.account.name || accountId;
};
