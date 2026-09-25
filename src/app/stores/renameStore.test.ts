import { beforeEach, describe, expect, test } from 'bun:test';
import { renameFailureMessage, renameFailureRecord, useRenameStore } from './renameStore';
import { useTransferErrorStore } from './transferErrorStore';

const rename = () => useRenameStore.getState();
const errors = () => useTransferErrorStore.getState();

const DENIED = 'photos/a.jpg: AccessDenied: Access Denied';
const MISSING = 'photos/b.jpg: NoSuchKey: The specified key does not exist.';

beforeEach(() => {
  useRenameStore.setState({ batches: [], lastCompletedAt: 0 });
  errors().clear();
  rename().startBatch({ id: 'b1', label: 'photos/ → archive/', total: 5 });
});

describe('finishBatch', () => {
  test('a partial batch reports every reason, one per line', () => {
    rename().finishBatch('b1', { moved: 3, failed: 2, errors: [DENIED, MISSING] });

    expect(errors().failures).toHaveLength(1);
    const [failure] = errors().failures;
    expect(failure.id).toBe('rename:b1');
    expect(failure.kind).toBe('rename');
    expect(failure.name).toBe('photos/ → archive/');
    expect(failure.message).toBe(`${DENIED}\n${MISSING}`);
    expect(failure.progress).toEqual({ done: 3, total: 5 });
    expect(errors().isOpen).toBe(true);
    // The batch holds the same text, so the dock rebuilds the same record.
    expect(rename().batches[0].status).toBe('partial');
    expect(rename().batches[0].error).toBe(failure.message);
  });

  test('a batch where nothing moved reports as an error', () => {
    rename().finishBatch('b1', { moved: 0, failed: 5, errors: [DENIED, MISSING] });

    expect(rename().batches[0].status).toBe('error');
    expect(errors().failures[0].message).toBe(`${DENIED}\n${MISSING}`);
    expect(errors().failures[0].progress).toEqual({ done: 0, total: 5 });
  });

  test('a successful batch reports nothing', () => {
    rename().finishBatch('b1', { moved: 5, failed: 0, errors: [] });

    expect(rename().batches[0].status).toBe('success');
    expect(rename().batches[0].error).toBeUndefined();
    expect(errors().failures).toHaveLength(0);
    expect(errors().isOpen).toBe(false);
  });

  test('failures without reasons still say how many files failed', () => {
    rename().finishBatch('b1', { moved: 4, failed: 1, errors: [] });
    expect(errors().failures[0].message).toBe('1 file could not be renamed');
  });

  test('a batch the store no longer holds reports nothing', () => {
    rename().finishBatch('gone', { moved: 0, failed: 2, errors: [DENIED] });
    expect(errors().failures).toHaveLength(0);
  });
});

describe('failBatch', () => {
  test('reports the error the whole batch stopped with, and what it renamed first', () => {
    rename().handleProgressEvent({
      batch_id: 'b1',
      completed: 2,
      total: 5,
      failed: 1,
      current_key: 'photos/c.jpg',
      ops_per_sec: 3,
      eta_ms: 900,
      done: false,
    });
    rename().failBatch('b1', 'transient: GET: try again');

    expect(rename().batches[0].status).toBe('error');
    const [failure] = errors().failures;
    expect(failure.id).toBe('rename:b1');
    expect(failure.message).toBe('transient: GET: try again');
    expect(failure.progress).toEqual({ done: 1, total: 5 });
  });

  test('a blank error still reads as a failure', () => {
    rename().failBatch('b1', '');
    expect(errors().failures[0].message).toBe('Rename failed');
  });
});

describe('renameFailureRecord', () => {
  test('is null while a batch runs and after it succeeds', () => {
    const batch = rename().batches[0];
    expect(renameFailureRecord(batch)).toBeNull();
    expect(renameFailureRecord({ ...batch, status: 'success' })).toBeNull();
  });

  test('rebuilds the reported record from the batch, as the dock does', () => {
    rename().finishBatch('b1', { moved: 3, failed: 2, errors: [DENIED, MISSING] });

    const reported = errors().failures[0];
    expect(renameFailureRecord(rename().batches[0], reported.occurredAt)).toEqual(reported);
  });
});

describe('renameFailureMessage', () => {
  test('joins every reason, not just the first', () => {
    expect(renameFailureMessage({ moved: 0, failed: 3, errors: ['a', 'b', 'c'] })).toBe('a\nb\nc');
  });

  test('counts the files when the backend sent no reasons', () => {
    expect(renameFailureMessage({ moved: 1, failed: 2, errors: [] })).toBe(
      '2 files could not be renamed'
    );
  });
});
