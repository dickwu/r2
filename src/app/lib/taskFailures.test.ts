import { describe, expect, test } from 'bun:test';
import type { TransferFailure } from '@/app/lib/transferFailure';
import type { ProviderAccount } from '@/app/stores/accountStore';
import type { DownloadTask } from '@/app/stores/downloadStore';
import type { MoveTask } from '@/app/stores/moveStore';
import type { UploadTask } from '@/app/stores/uploadStore';
import {
  accountDisplayName,
  downloadFailure,
  moveFailure,
  preferRecorded,
  uploadFailure,
} from './taskFailures';

const AT = 1_790_000_000_000;

const upload: UploadTask = {
  id: 'upload-1',
  filePath: '/Users/me/Pictures/IMG_2214.CR2',
  fileName: 'IMG_2214.CR2',
  renamedFileName: 'IMG_2214 (1).CR2',
  fileSize: 1000,
  contentType: 'image/x-canon-cr2',
  status: 'error',
  progress: 62,
  speed: 0,
  error: 'AccessDenied: Access Denied',
};

const download: DownloadTask = {
  id: 'download-1',
  key: 'photos/2026/IMG_2214.CR2',
  fileName: 'IMG_2214.CR2',
  fileSize: 1000,
  localPath: '/Users/me/Downloads',
  bucket: 'media',
  status: 'error',
  progress: 40,
  downloadedBytes: 400,
  speed: 0,
  error: 'NoSuchKey: The specified key does not exist.',
  chunks: [],
  speedHistory: [],
  peakSpeed: 0,
};

const move: MoveTask = {
  id: 'move-1',
  sourceKey: 'photos/2026/IMG_2214.CR2',
  destKey: 'archive/IMG_2214.CR2',
  sourceBucket: 'media',
  sourceAccountId: 'acct',
  sourceProvider: 'r2',
  destBucket: 'cold',
  destAccountId: 'acct',
  destProvider: 'r2',
  deleteOriginal: true,
  fileSize: 1000,
  progress: 40,
  transferredBytes: 400,
  speed: 0,
  phase: 'uploading',
  status: 'error',
  error: 'transient: PUT: try again',
};

const r2Account = (id: string, name: string | null): ProviderAccount => ({
  provider: 'r2',
  account: { id, name, created_at: 0, updated_at: 0 },
  tokens: [],
});

describe('uploadFailure', () => {
  test('names the local file and carries the destination the upload was given', () => {
    const ctx = {
      occurredAt: AT,
      key: 'photos/IMG_2214 (1).CR2',
      bucket: 'media',
      account: 'Greenwoods R2',
    };
    expect(uploadFailure(upload, ctx)).toEqual({
      id: 'upload:upload-1',
      kind: 'upload',
      name: 'IMG_2214.CR2',
      message: 'AccessDenied: Access Denied',
      occurredAt: AT,
      key: 'photos/IMG_2214 (1).CR2',
      bucket: 'media',
      account: 'Greenwoods R2',
      progress: { done: 620, total: 1000 },
    });
  });

  test('falls back to a plain message and leaves unknown context out', () => {
    const failure = uploadFailure({ ...upload, error: undefined }, { occurredAt: AT });
    expect(failure.message).toBe('Upload failed');
    expect(failure.key).toBeUndefined();
    expect(failure.bucket).toBeUndefined();
    expect(failure.account).toBeUndefined();
  });
});

describe('downloadFailure', () => {
  test('carries the key, the bucket and the bytes downloaded before it stopped', () => {
    expect(downloadFailure(download, { occurredAt: AT })).toEqual({
      id: 'download:download-1',
      kind: 'download',
      name: 'IMG_2214.CR2',
      message: 'NoSuchKey: The specified key does not exist.',
      occurredAt: AT,
      key: 'photos/2026/IMG_2214.CR2',
      bucket: 'media',
      progress: { done: 400, total: 1000 },
    });
  });

  test('falls back to a plain message and stamps the time it was seen', () => {
    const before = Date.now();
    const failure = downloadFailure({ ...download, error: undefined, bucket: undefined });
    expect(failure.message).toBe('Download failed');
    expect(failure.bucket).toBeUndefined();
    expect(before).toBeLessThanOrEqual(failure.occurredAt);
  });
});

describe('moveFailure', () => {
  test('names the source file, keys it source → destination, in the destination bucket', () => {
    expect(moveFailure(move, { occurredAt: AT })).toEqual({
      id: 'move:move-1',
      kind: 'move',
      name: 'IMG_2214.CR2',
      message: 'transient: PUT: try again',
      occurredAt: AT,
      key: 'photos/2026/IMG_2214.CR2 → archive/IMG_2214.CR2',
      bucket: 'cold',
      progress: { done: 400, total: 1000 },
    });
  });

  test('a move restored from the database takes its progress from the percent', () => {
    const restored = { ...move, transferredBytes: 0, progress: 60 };
    expect(moveFailure(restored).progress).toEqual({ done: 600, total: 1000 });
  });

  test('carries the recovery label as the stage, and a plain message when there is none', () => {
    const waiting = { ...move, status: 'needs_auth' as const, error: undefined };
    const failure = moveFailure(waiting, { stage: 'Credentials need attention' });
    expect(failure.stage).toBe('Credentials need attention');
    expect(failure.message).toBe('Move failed');
  });

  test('a recovery note is its own record, titled as the status bar puts it', () => {
    const waiting = { ...move, status: 'needs_auth' as const };
    const note = moveFailure(waiting, { stage: 'Credentials need attention' });
    expect(note.id).toBe('move-recovery:move-1');
    expect(note.title).toBe('Move needs attention');
    // A later outright failure with the same text is a different record, so it still opens.
    expect(moveFailure(move).id).toBe('move:move-1');
    expect(moveFailure(move).title).toBeUndefined();
  });
});

describe('preferRecorded', () => {
  const recorded: TransferFailure = uploadFailure(upload, {
    occurredAt: AT,
    key: 'photos/IMG_2214 (1).CR2',
    bucket: 'media',
  });

  test('re-opens the reported record while its message is unchanged', () => {
    const fresh = uploadFailure(upload, { key: 'elsewhere/IMG_2214 (1).CR2', bucket: 'media' });
    expect(preferRecorded(fresh, [recorded])).toBe(recorded);
  });

  test('uses the fresh record when the message changed or nothing was reported', () => {
    const changed = uploadFailure({ ...upload, error: 'SlowDown: Reduce your request rate.' });
    expect(preferRecorded(changed, [recorded])).toBe(changed);
    const fresh = uploadFailure(upload);
    expect(preferRecorded(fresh, [])).toBe(fresh);
  });
});

describe('accountDisplayName', () => {
  const accounts = [r2Account('cf-1', 'Greenwoods R2'), r2Account('cf-2', null)];

  test('is the account name, else its id', () => {
    expect(accountDisplayName(accounts, 'r2', 'cf-1')).toBe('Greenwoods R2');
    expect(accountDisplayName(accounts, 'r2', 'cf-2')).toBe('cf-2');
  });

  test('falls back to the id for an unknown account or another provider', () => {
    expect(accountDisplayName(accounts, 'r2', 'cf-9')).toBe('cf-9');
    expect(accountDisplayName(accounts, 'aws', 'cf-1')).toBe('cf-1');
  });
});
