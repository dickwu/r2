import { expect, test } from 'bun:test';
import {
  useMoveStore,
  isMoveAwaitingAction,
  selectAttentionCount,
  type MoveSession,
} from './moveStore';

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
