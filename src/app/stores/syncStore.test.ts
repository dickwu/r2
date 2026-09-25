import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { syncFailureRecord, useSyncStore } from './syncStore';
import { useTransferErrorStore } from './transferErrorStore';

const sync = () => useSyncStore.getState();
const errors = () => useTransferErrorStore.getState();

// Counts report() calls rather than records: a repeat report replaces its
// record in place, so the record count alone cannot tell once from twice.
let reports = 0;
let unsubscribe: () => void = () => undefined;

beforeEach(() => {
  sync().reset();
  errors().clear();
  reports = 0;
  unsubscribe = useTransferErrorStore.subscribe((state, prev) => {
    if (state.failures !== prev.failures) reports += 1;
  });
});

afterEach(() => unsubscribe());

describe('failBackgroundSync', () => {
  test('reports the failure once, named after the bucket, and opens the modal', () => {
    sync().setCurrentBucket('r2:acct-1:ns', 'photos');
    sync().startBackgroundSync();
    sync().failBackgroundSync('AccessDenied: Access Denied');

    expect(sync().backgroundSync.isRunning).toBe(false);
    expect(sync().backgroundSync.error).toBe('AccessDenied: Access Denied');
    expect(reports).toBe(1);
    expect(errors().failures).toHaveLength(1);
    const [failure] = errors().failures;
    expect(failure.id).toBe('sync:r2:acct-1:ns/photos');
    expect(failure.kind).toBe('sync');
    expect(failure.name).toBe('photos');
    expect(failure.bucket).toBe('photos');
    expect(failure.message).toBe('AccessDenied: Access Denied');
    expect(errors().isOpen).toBe(true);
    expect(errors().focusedId).toBe('sync:r2:acct-1:ns/photos');
  });

  test('failing again the same way keeps one record and leaves a closed modal closed', () => {
    sync().setCurrentBucket('r2:acct-1:ns', 'photos');
    sync().failBackgroundSync('SlowDown: Please reduce your request rate.');
    errors().close();
    sync().startBackgroundSync();
    sync().failBackgroundSync('SlowDown: Please reduce your request rate.');

    expect(reports).toBe(2);
    expect(errors().failures).toHaveLength(1);
    expect(errors().isOpen).toBe(false);
  });

  test('a different reason reopens the modal on the same record', () => {
    sync().setCurrentBucket('r2:acct-1:ns', 'photos');
    sync().failBackgroundSync('SlowDown: Please reduce your request rate.');
    errors().close();
    sync().failBackgroundSync('AccessDenied: Access Denied');

    expect(errors().failures).toHaveLength(1);
    expect(errors().failures[0].message).toBe('AccessDenied: Access Denied');
    expect(errors().isOpen).toBe(true);
  });

  test('each bucket keeps its own record', () => {
    sync().setCurrentBucket('r2:acct-1:ns', 'photos');
    sync().failBackgroundSync('connection reset');
    sync().setCurrentBucket('r2:acct-1:ns', 'backups');
    sync().resetBackgroundSync();
    sync().failBackgroundSync('connection reset');

    expect(errors().failures.map((f) => f.id)).toEqual([
      'sync:r2:acct-1:ns/photos',
      'sync:r2:acct-1:ns/backups',
    ]);
  });

  test('without a current bucket the record falls back to a generic id and name', () => {
    sync().failBackgroundSync('no route to host');

    const [failure] = errors().failures;
    expect(failure.id).toBe('sync:current');
    expect(failure.name).toBe('Bucket sync');
    expect(failure.bucket).toBeUndefined();
  });

  test('an empty error is not a failure, just as the sync pill reads it', () => {
    sync().failBackgroundSync('');

    expect(reports).toBe(0);
    expect(errors().failures).toHaveLength(0);
    expect(errors().isOpen).toBe(false);
  });
});

describe('completeBackgroundSync', () => {
  test('ends the failure record, so the next failure the same way is news again', () => {
    sync().setCurrentBucket('r2:acct-1:ns', 'photos');
    sync().failBackgroundSync('connection reset');
    errors().close();
    sync().startBackgroundSync();
    sync().completeBackgroundSync(10);
    expect(errors().failures).toHaveLength(0);

    sync().startBackgroundSync();
    sync().failBackgroundSync('connection reset');
    expect(errors().failures).toHaveLength(1);
    expect(errors().isOpen).toBe(true);
  });

  test('clears the error a failed run left and ends its record, reporting nothing itself', () => {
    sync().setCurrentBucket('r2:acct-1:ns', 'photos');
    sync().failBackgroundSync('connection reset');
    sync().completeBackgroundSync(10, 2048);

    expect(sync().backgroundSync.error).toBeNull();
    expect(sync().backgroundSync.isRunning).toBe(false);
    expect(sync().backgroundSync.objectsFetched).toBe(10);
    // A run that finished settles the failure: nothing is left to explain.
    expect(errors().failures).toHaveLength(0);
    expect(errors().isOpen).toBe(false);
  });
});

describe('syncFailureRecord', () => {
  test('is null while the sync has not failed', () => {
    sync().setCurrentBucket('r2:acct-1:ns', 'photos');
    expect(syncFailureRecord(sync())).toBeNull();
  });

  test('rebuilds the record the store reported, as the sync pill does', () => {
    sync().setCurrentBucket('r2:acct-1:ns', 'photos');
    sync().failBackgroundSync('AccessDenied: Access Denied');

    const reported = errors().failures[0];
    expect(syncFailureRecord(sync(), reported.occurredAt)).toEqual(reported);
  });

  test('splits the key at its last colon: account keys hold colons, bucket names never do', () => {
    const record = syncFailureRecord(
      { currentBucketKey: 'minio:acct:9f3a:media-2026', backgroundSync: { error: 'boom' } },
      1_000
    );

    expect(record?.id).toBe('sync:minio:acct:9f3a/media-2026');
    expect(record?.bucket).toBe('media-2026');
    expect(record?.occurredAt).toBe(1_000);
  });

  test('a blank reason still reads as a failure', () => {
    const record = syncFailureRecord({ currentBucketKey: null, backgroundSync: { error: '   ' } });
    expect(record?.message).toBe('Sync failed');
  });
});
