import { beforeEach, describe, expect, test } from 'bun:test';
import {
  useMoveStore,
  isMoveAwaitingAction,
  moveFailureFromEvent,
  selectAttentionCount,
  MOVE_RECOVERY_LABELS,
  type MoveSession,
} from './moveStore';
import { moveFailure } from '@/app/lib/taskFailures';
import { useTransferErrorStore } from './transferErrorStore';

const session: MoveSession = {
  id: 'recovery',
  source_key: 'a',
  dest_key: 'b',
  source_bucket: 'source',
  source_account_id: 'account',
  source_provider: 'r2',
  dest_bucket: 'target',
  dest_account_id: 'account',
  dest_provider: 'r2',
  delete_original: true,
  file_size: 4,
  progress: 100,
  status: 'delete_pending',
  error: 'source retained',
  created_at: 1,
  updated_at: 1,
};

test('durable move recovery statuses are preserved and never cleared as completed tasks', () => {
  for (const status of [
    'delete_pending',
    'outcome_unknown',
    'needs_auth',
    'conflict',
    'needs_action',
  ] as const) {
    useMoveStore.getState().loadFromDatabase([{ ...session, status }]);
    expect(useMoveStore.getState().tasks[0].status).toBe(status);
    expect(isMoveAwaitingAction(status)).toBe(true);
    expect(selectAttentionCount(useMoveStore.getState())).toBe(1);
    useMoveStore.getState().clearFinishedTasks();
    expect(useMoveStore.getState().tasks).toHaveLength(1);
    useMoveStore
      .getState()
      .handleStatusChanged({ task_id: session.id, status: 'pending', error: null });
    expect(useMoveStore.getState().tasks[0].status).toBe('pending');
  }
  useMoveStore.getState().clearAllTasks();
});

describe('move failures reach the failure modal on the way into the terminal error only', () => {
  const RECOVERY_STATUSES = [
    'delete_pending',
    'outcome_unknown',
    'needs_auth',
    'conflict',
    'needs_action',
  ] as const;

  const uploading: MoveSession = {
    ...session,
    id: 'move-1',
    source_key: 'photos/2026/IMG_2214.CR2',
    dest_key: 'archive/IMG_2214.CR2',
    file_size: 1000,
    progress: 40,
    status: 'uploading',
    error: null,
  };

  const statusChanged = (status: string, error: string | null = null) =>
    useMoveStore.getState().handleStatusChanged({ task_id: uploading.id, status, error });

  const failures = () => useTransferErrorStore.getState().failures;

  /** How many times `run` reported to the failure store: every report() is one store update. */
  const countReports = (run: () => void): number => {
    let reports = 0;
    const unsubscribe = useTransferErrorStore.subscribe(() => {
      reports += 1;
    });
    try {
      run();
    } finally {
      unsubscribe();
    }
    return reports;
  };

  beforeEach(() => {
    useMoveStore.getState().clearAllTasks();
    useTransferErrorStore.getState().clear();
  });

  test('a move entering error reports once, keyed source → destination in the destination bucket', () => {
    useMoveStore.getState().loadFromDatabase([uploading]);
    expect(countReports(() => statusChanged('error', 'AccessDenied: Access Denied'))).toBe(1);

    expect(useTransferErrorStore.getState().isOpen).toBe(true);
    const [failure] = failures();
    expect(failures()).toHaveLength(1);
    expect(failure).toEqual({
      id: 'move:move-1',
      kind: 'move',
      name: 'IMG_2214.CR2',
      message: 'AccessDenied: Access Denied',
      occurredAt: failure.occurredAt,
      key: 'photos/2026/IMG_2214.CR2 → archive/IMG_2214.CR2',
      bucket: 'target',
      progress: { done: 400, total: 1000 },
    });
  });

  test('the same error event again does not report again', () => {
    useMoveStore.getState().loadFromDatabase([uploading]);
    statusChanged('error', 'AccessDenied: Access Denied');
    useTransferErrorStore.getState().clear();

    expect(countReports(() => statusChanged('error', 'AccessDenied: Access Denied'))).toBe(0);
    expect(failures()).toHaveLength(0);
  });

  test('recovery statuses neither report nor open the failure modal', () => {
    for (const status of RECOVERY_STATUSES) {
      useMoveStore.getState().loadFromDatabase([uploading]);
      expect(countReports(() => statusChanged(status, 'AccessDenied: Access Denied'))).toBe(0);
      expect(useMoveStore.getState().tasks[0].status).toBe(status);
    }
    expect(failures()).toHaveLength(0);
    expect(useTransferErrorStore.getState().isOpen).toBe(false);
  });

  test('a failure restored from the database never reports, then or on its next event', () => {
    const restored = { ...uploading, status: 'error', error: 'AccessDenied: Access Denied' };
    expect(countReports(() => useMoveStore.getState().loadFromDatabase([restored]))).toBe(0);
    expect(countReports(() => statusChanged('error', 'AccessDenied: Access Denied'))).toBe(0);
    expect(useTransferErrorStore.getState().isOpen).toBe(false);
  });

  test('a late error event after the move succeeded is ignored and reports nothing', () => {
    useMoveStore.getState().loadFromDatabase([{ ...uploading, status: 'success' }]);
    expect(countReports(() => statusChanged('error', 'late'))).toBe(0);
    expect(useMoveStore.getState().tasks[0].status).toBe('success');
  });

  test('a retried move that fails again the same way opens the modal again', () => {
    useMoveStore.getState().loadFromDatabase([uploading]);
    statusChanged('error', 'AccessDenied: Access Denied');
    useTransferErrorStore.getState().close();

    statusChanged('pending');
    expect(failures()).toHaveLength(0);

    statusChanged('error', 'AccessDenied: Access Denied');
    expect(failures()).toHaveLength(1);
    expect(useTransferErrorStore.getState().isOpen).toBe(true);
  });

  test('a reload that lands before the status event reports the failure once', () => {
    useMoveStore.getState().loadFromDatabase([uploading]);
    const reports = countReports(() => {
      useMoveStore
        .getState()
        .loadFromDatabase([
          { ...uploading, status: 'error', error: 'AccessDenied: Access Denied' },
        ]);
      statusChanged('error', 'AccessDenied: Access Denied');
    });
    expect(reports).toBe(1);
    expect(failures()).toHaveLength(1);
    expect(failures()[0].id).toBe('move:move-1');
  });

  test('a stale reload that still shows the move running does not erase a fresh record', () => {
    useMoveStore.getState().loadFromDatabase([uploading]);
    statusChanged('error', 'AccessDenied: Access Denied');
    useMoveStore.getState().loadFromDatabase([uploading]);
    expect(failures()).toHaveLength(1);
    expect(useTransferErrorStore.getState().isOpen).toBe(true);
  });

  test('a failure the store first hears of is recorded from the event itself', () => {
    const reports = countReports(() =>
      useMoveStore.getState().handleStatusChanged({
        task_id: 'move-9',
        status: 'error',
        error: 'NoSuchKey: The specified key does not exist.',
        source_bucket: 'source',
        source_key: 'photos/2026/IMG_9.CR2',
        dest_bucket: 'cold',
        dest_key: 'archive/IMG_9.CR2',
      })
    );
    expect(reports).toBe(1);
    const [failure] = failures();
    expect(failure).toEqual({
      id: 'move:move-9',
      kind: 'move',
      name: 'IMG_9.CR2',
      message: 'NoSuchKey: The specified key does not exist.',
      occurredAt: failure.occurredAt,
      key: 'photos/2026/IMG_9.CR2 → archive/IMG_9.CR2',
      bucket: 'cold',
    });
    expect(useTransferErrorStore.getState().isOpen).toBe(true);
  });

  test('a move that stops waiting on the person ends its recovery note', () => {
    useMoveStore.getState().loadFromDatabase([uploading]);
    statusChanged('needs_auth', 'AccessDenied: Access Denied');
    const waiting = useMoveStore.getState().tasks[0];
    useTransferErrorStore
      .getState()
      .show(moveFailure(waiting, { stage: MOVE_RECOVERY_LABELS[waiting.status] }));
    expect(failures().map((f) => f.id)).toEqual(['move-recovery:move-1']);

    statusChanged('success');
    expect(failures()).toHaveLength(0);
  });
});

describe('moveFailureFromEvent', () => {
  test('keys the move source → destination in the destination bucket, without a fault line', () => {
    const failure = moveFailureFromEvent(
      {
        task_id: 'move-9',
        status: 'error',
        error: 'AccessDenied: Access Denied',
        source_key: 'photos/2026/IMG_9.CR2',
        dest_bucket: 'cold',
        dest_key: 'archive/IMG_9.CR2',
      },
      5_000
    );
    expect(failure.key).toBe('photos/2026/IMG_9.CR2 → archive/IMG_9.CR2');
    expect(failure.name).toBe('IMG_9.CR2');
    expect(failure.bucket).toBe('cold');
    expect(failure.occurredAt).toBe(5_000);
    expect(failure.progress).toBeUndefined();
  });

  test('a bare event still names the move and carries a plain message', () => {
    const failure = moveFailureFromEvent({ task_id: 'move-9', status: 'error', error: null });
    expect(failure.id).toBe('move:move-9');
    expect(failure.name).toBe('Move');
    expect(failure.message).toBe('Move failed');
    expect(failure.key).toBeUndefined();
  });
});
