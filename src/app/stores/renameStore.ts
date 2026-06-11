import { create } from 'zustand';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { batchMoveObjects, BatchMoveResult, MoveOperation, StorageConfig } from '@/app/lib/r2cache';

// Global listener state - persists across component unmounts
let globalListenersSetup = false;
let globalUnlisteners: UnlistenFn[] = [];

/** Enriched `batch-move-progress` payload from the Rust batch executor. */
export interface BatchMoveProgressEvent {
  batch_id: string;
  completed: number;
  total: number;
  failed: number;
  current_key: string;
  ops_per_sec: number;
  eta_ms: number;
  done: boolean;
}

export type RenameBatchStatus = 'running' | 'success' | 'partial' | 'error';

export interface RenameBatch {
  id: string;
  /** Human-readable description, e.g. "photos/ → archive/". */
  label: string;
  total: number;
  completed: number;
  failed: number;
  currentKey: string;
  opsPerSec: number;
  etaMs: number;
  status: RenameBatchStatus;
  error?: string;
  startedAt: number;
}

interface RenameStore {
  batches: RenameBatch[];
  /** Bumped whenever a batch reaches a terminal state — UI refresh trigger. */
  lastCompletedAt: number;

  startBatch: (init: { id: string; label: string; total: number }) => void;
  handleProgressEvent: (event: BatchMoveProgressEvent) => void;
  finishBatch: (id: string, result: BatchMoveResult) => void;
  failBatch: (id: string, error: string) => void;
  removeBatch: (id: string) => void;
  clearFinished: () => void;
}

function isFinished(status: RenameBatchStatus): boolean {
  return status !== 'running';
}

export const useRenameStore = create<RenameStore>((set) => ({
  batches: [],
  lastCompletedAt: 0,

  startBatch: ({ id, label, total }) => {
    const batch: RenameBatch = {
      id,
      label,
      total,
      completed: 0,
      failed: 0,
      currentKey: '',
      opsPerSec: 0,
      etaMs: 0,
      status: 'running',
      startedAt: Date.now(),
    };
    set((state) => ({ batches: [...state.batches, batch] }));
  },

  handleProgressEvent: (event) => {
    set((state) => ({
      batches: state.batches.map((b) => {
        if (b.id !== event.batch_id || isFinished(b.status)) return b;
        return {
          ...b,
          completed: Math.max(b.completed, event.completed),
          failed: event.failed,
          currentKey: event.current_key || b.currentKey,
          opsPerSec: event.ops_per_sec,
          etaMs: event.eta_ms,
        };
      }),
    }));
  },

  finishBatch: (id, result) => {
    set((state) => ({
      lastCompletedAt: Date.now(),
      batches: state.batches.map((b) => {
        if (b.id !== id) return b;
        const status: RenameBatchStatus =
          result.failed === 0 ? 'success' : result.moved > 0 ? 'partial' : 'error';
        return {
          ...b,
          completed: result.moved + result.failed,
          failed: result.failed,
          opsPerSec: 0,
          etaMs: 0,
          status,
          error: result.errors[0],
        };
      }),
    }));
  },

  failBatch: (id, error) => {
    set((state) => ({
      lastCompletedAt: Date.now(),
      batches: state.batches.map((b) =>
        b.id === id ? { ...b, status: 'error' as const, error, opsPerSec: 0, etaMs: 0 } : b
      ),
    }));
  },

  removeBatch: (id) => {
    set((state) => ({ batches: state.batches.filter((b) => b.id !== id) }));
  },

  clearFinished: () => {
    set((state) => ({ batches: state.batches.filter((b) => !isFinished(b.status)) }));
  },
}));

export const selectActiveRenameCount = (state: RenameStore) =>
  state.batches.filter((b) => b.status === 'running').length;

function generateBatchId(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID();
  }
  return `batch-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
}

/**
 * Setup the global rename progress listener. Should be called once on app
 * initialization; survives component unmounts so background renames keep
 * reporting into the store (and the transfer dock).
 */
export async function setupGlobalRenameListeners(): Promise<void> {
  if (globalListenersSetup) return;
  globalListenersSetup = true;

  try {
    const unlistenProgress = await listen<BatchMoveProgressEvent>(
      'batch-move-progress',
      (event) => {
        useRenameStore.getState().handleProgressEvent(event.payload);
      }
    );
    globalUnlisteners.push(unlistenProgress);
  } catch (e) {
    console.error('Failed to setup global rename listeners:', e);
    globalListenersSetup = false;
  }
}

/**
 * Run a batch rename and track it in the store. `done` resolves with the
 * backend result; the store keeps reporting progress even if the initiating
 * modal unmounts, so the operation is safe to continue in the background.
 */
export function runRenameBatch(
  config: StorageConfig,
  operations: MoveOperation[],
  label: string
): { id: string; done: Promise<BatchMoveResult> } {
  const id = generateBatchId();
  useRenameStore.getState().startBatch({ id, label, total: operations.length });
  const done = batchMoveObjects(config, operations, id).then(
    (result) => {
      useRenameStore.getState().finishBatch(id, result);
      return result;
    },
    (e: unknown) => {
      const message = e instanceof Error ? e.message : String(e);
      useRenameStore.getState().failBatch(id, message);
      throw e;
    }
  );
  return { id, done };
}
