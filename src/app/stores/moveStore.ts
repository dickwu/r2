import { create } from 'zustand';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { createProgressBatcher, smoothSpeed } from '@/app/lib/progressThrottle';
import { moveFailure } from '@/app/lib/taskFailures';
import type { TransferFailure } from '@/app/lib/transferFailure';
import { useTransferErrorStore } from '@/app/stores/transferErrorStore';

export const MAX_CONCURRENT_MOVES = 5;

// Coalescing window for high-frequency progress events
const PROGRESS_THROTTLE_MS = 200;

// Global listener state - persists across component unmounts
let globalListenersSetup = false;
let globalUnlisteners: UnlistenFn[] = [];

export interface MoveProgressEvent {
  task_id: string;
  phase: string;
  percent: number;
  transferred_bytes: number;
  total_bytes: number;
  speed: number;
}

export interface MoveStatusChangedEvent {
  task_id: string;
  status: string;
  error: string | null;
  source_provider?: string;
  source_account_id?: string;
  source_bucket?: string;
  source_key?: string;
  dest_provider?: string;
  dest_account_id?: string;
  dest_bucket?: string;
  dest_key?: string;
  delete_original?: boolean;
}

export interface MoveTaskDeletedEvent {
  task_id: string;
}

export interface MoveBatchOperationEvent {
  operation: string; // "clear_finished" | "clear_all" | "pause_all" | "resume_all"
  source_bucket: string;
  source_account_id: string;
}

export type MoveStatus =
  | 'pending'
  | 'downloading'
  | 'uploading'
  | 'finishing'
  | 'deleting'
  | 'paused'
  | 'delete_pending'
  | 'outcome_unknown'
  | 'needs_auth'
  | 'conflict'
  | 'needs_action'
  | 'success'
  | 'error'
  | 'cancelled';

export interface MoveTask {
  id: string;
  sourceKey: string;
  destKey: string;
  sourceBucket: string;
  sourceAccountId: string;
  sourceProvider: string;
  destBucket: string;
  destAccountId: string;
  destProvider: string;
  deleteOriginal: boolean;
  fileSize: number;
  progress: number;
  transferredBytes: number;
  speed: number;
  phase: string;
  status: MoveStatus;
  error?: string;
}

export interface MoveSession {
  id: string;
  source_key: string;
  dest_key: string;
  source_bucket: string;
  source_account_id: string;
  source_provider: string;
  dest_bucket: string;
  dest_account_id: string;
  dest_provider: string;
  delete_original: boolean;
  file_size: number;
  progress: number;
  status: string;
  error: string | null;
  created_at: number;
  updated_at: number;
}

interface MoveStore {
  tasks: MoveTask[];
  modalOpen: boolean;

  setModalOpen: (open: boolean) => void;
  loadFromDatabase: (sessions: MoveSession[]) => void;
  clearAllTasks: () => void;
  clearFinishedTasks: () => void;
  handleProgressEvent: (event: MoveProgressEvent) => void;
  handleStatusChanged: (event: MoveStatusChangedEvent) => void;
  handleTaskDeleted: (event: MoveTaskDeletedEvent) => void;
}

function mapStatus(dbStatus: string): MoveStatus {
  switch (dbStatus) {
    case 'pending':
      return 'pending';
    case 'downloading':
      return 'downloading';
    case 'uploading':
      return 'uploading';
    case 'finishing':
      return 'finishing';
    case 'deleting':
      return 'deleting';
    case 'paused':
      return 'paused';
    case 'delete_pending':
    case 'outcome_unknown':
    case 'needs_auth':
    case 'conflict':
    case 'needs_action':
      return dbStatus;
    case 'success':
      return 'success';
    case 'error':
      return 'error';
    case 'cancelled':
      return 'cancelled';
    default:
      return 'pending';
  }
}

function derivePhase(status: MoveStatus, existingPhase?: string): string {
  // Active statuses should always update the phase
  switch (status) {
    case 'downloading':
    case 'uploading':
    case 'deleting':
    case 'finishing':
      return status;
    case 'success':
    case 'error':
    case 'cancelled':
      // Keep the last active phase for completed tasks
      return existingPhase || 'uploading';
    default:
      return existingPhase || 'pending';
  }
}

export function isMoveAwaitingAction(status: string): boolean {
  return (
    status === 'delete_pending' ||
    status === 'outcome_unknown' ||
    status === 'needs_auth' ||
    status === 'conflict' ||
    status === 'needs_action'
  );
}

export const MOVE_RECOVERY_LABELS: Record<string, string> = {
  delete_pending: 'Copy verified; source retained until deletion succeeds',
  outcome_unknown: 'Remote result uncertain; resume to reconcile before continuing',
  needs_auth: 'Credentials or permissions need attention; source deletion stopped',
  conflict: 'Object changed; source deletion stopped for review',
  needs_action: 'Copy or cleanup needs review; the recovery record is retained',
};

/**
 * The failure modal follows a move's step INTO the terminal 'error', whichever
 * path carried it — a status event or a database reload. Only that step
 * reports: the recovery statuses wait on the person and never open the modal,
 * a repeated error event changes nothing, and a failure restored at startup
 * has no earlier step to compare with.
 */
function reportFailureStep(before: MoveTask | undefined, after: MoveTask | undefined): void {
  if (!before || !after) return;
  if (before.status !== 'error' && after.status === 'error') {
    useTransferErrorStore.getState().report(moveFailure(after));
  }
}

/**
 * A move that left its failure (a retry goes back to pending) ends its record,
 * so failing again is a new event; one that stopped waiting on the person ends
 * its recovery note. Only a status event says so: a database reload may carry
 * a snapshot taken before the failure was written, and must not erase it.
 */
function forgetFailureStep(before: MoveTask | undefined, after: MoveTask | undefined): void {
  if (!before || !after) return;
  const errors = useTransferErrorStore.getState();
  if (before.status === 'error' && after.status !== 'error') errors.forget(`move:${after.id}`);
  if (isMoveAwaitingAction(before.status) && !isMoveAwaitingAction(after.status)) {
    errors.forget(`move-recovery:${after.id}`);
  }
}

/** "photos/2026/IMG_2214.CR2" → "IMG_2214.CR2". */
const baseName = (key: string): string => key.split('/').filter(Boolean).pop() ?? key;

/**
 * The record for a move the store first meets through its failure event. The
 * event carries the keys and buckets; the bytes are unknown, so there is no
 * fault line. A reload could not supply it: the active-tasks query leaves
 * failed moves out.
 */
export function moveFailureFromEvent(
  event: MoveStatusChangedEvent,
  now: number = Date.now()
): TransferFailure {
  const sourceKey = event.source_key ?? '';
  const destKey = event.dest_key ?? '';
  return {
    id: `move:${event.task_id}`,
    kind: 'move',
    name: baseName(sourceKey) || 'Move',
    message: event.error || 'Move failed',
    occurredAt: now,
    key: sourceKey && destKey ? `${sourceKey} → ${destKey}` : sourceKey || undefined,
    bucket: event.dest_bucket,
  };
}

function isFinishedStatus(status: MoveStatus): boolean {
  return status === 'success' || status === 'error' || status === 'cancelled';
}

// Apply a progress update to a move task (monotonic while active, smoothed speed)
function applyMoveProgress(t: MoveTask, evt: MoveProgressEvent): MoveTask {
  if (isMoveAwaitingAction(t.status)) return t;
  // Only use Math.max if task is already active, not if just starting
  const isActive = t.status === 'downloading' || t.status === 'uploading';
  const newProgress = isActive ? Math.max(t.progress, evt.percent) : evt.percent;
  const newTransferred = isActive
    ? Math.max(t.transferredBytes, evt.transferred_bytes)
    : evt.transferred_bytes;
  const newFileSize = t.fileSize > 0 ? t.fileSize : evt.total_bytes;
  return {
    ...t,
    progress: newProgress,
    transferredBytes: newTransferred,
    fileSize: newFileSize,
    speed: smoothSpeed(t.speed, evt.speed),
    phase: evt.phase || t.phase,
    // Update status to match phase if task was pending
    status: t.status === 'pending' && evt.phase ? mapStatus(evt.phase) : t.status,
  };
}

export const useMoveStore = create<MoveStore>((set, get) => {
  // Coalesce bursts of progress events into single batched store updates
  const progressBatcher = createProgressBatcher<MoveProgressEvent>(
    PROGRESS_THROTTLE_MS,
    (updates) => {
      set((state) => ({
        tasks: state.tasks.map((t) => {
          const evt = updates.get(t.id);
          return evt ? applyMoveProgress(t, evt) : t;
        }),
      }));
    }
  );

  return {
    tasks: [],
    modalOpen: false,

    setModalOpen: (open) => {
      set({ modalOpen: open });
    },

    loadFromDatabase: (sessions) => {
      const currentTasks = get().tasks;
      const tasks: MoveTask[] = sessions.map((session) => {
        const existing = currentTasks.find((t) => t.id === session.id);
        const dbStatus = mapStatus(session.status);
        const dbProgress = Math.min(Math.max(session.progress || 0, 0), 100);

        // Determine if task just started (was pending, now active) - reset progress
        const taskJustStarted =
          existing?.status === 'pending' &&
          (dbStatus === 'downloading' || dbStatus === 'uploading');

        // Use existing progress only if task was already active (not just started)
        const progress = taskJustStarted
          ? dbProgress
          : existing
            ? Math.max(existing.progress, dbProgress)
            : dbProgress;

        return {
          id: session.id,
          sourceKey: session.source_key,
          destKey: session.dest_key,
          sourceBucket: session.source_bucket,
          sourceAccountId: session.source_account_id,
          sourceProvider: session.source_provider,
          destBucket: session.dest_bucket,
          destAccountId: session.dest_account_id,
          destProvider: session.dest_provider,
          deleteOriginal: session.delete_original,
          fileSize: session.file_size || 0,
          progress,
          transferredBytes: taskJustStarted ? 0 : existing?.transferredBytes || 0,
          speed: taskJustStarted ? 0 : existing?.speed || 0,
          phase: derivePhase(dbStatus, taskJustStarted ? undefined : existing?.phase),
          status: dbStatus,
          error: session.error || undefined,
        };
      });
      set({ tasks });
      // A reload can land between the worker's SQLite write and its status
      // event, so a known task's step into failure can show here first. A
      // startup restore knows no task yet and stays quiet.
      for (const task of tasks) {
        reportFailureStep(
          currentTasks.find((t) => t.id === task.id),
          task
        );
      }
    },

    clearAllTasks: () => {
      set({ tasks: [] });
    },

    clearFinishedTasks: () => {
      set((state) => ({
        tasks: state.tasks.filter((task) => !isFinishedStatus(task.status)),
      }));
    },

    handleProgressEvent: (event) => {
      // Check if task exists in store
      const taskExists = get().tasks.some((t) => t.id === event.task_id);
      if (!taskExists) {
        // Task not in store - reload from database to get it
        invoke<MoveSession[]>('get_all_active_move_tasks')
          .then((sessions) => {
            useMoveStore.getState().loadFromDatabase(sessions);
          })
          .catch((e) => console.error('Failed to reload tasks:', e));
        return;
      }

      progressBatcher.push(event.task_id, event);
    },

    handleStatusChanged: (event) => {
      const newStatus = mapStatus(event.status);
      const previous = get().tasks.find((t) => t.id === event.task_id);

      // A failure the store first hears of here has no task to update: the event
      // itself is the record's source (a reload would not bring the task either)
      if (!previous && newStatus === 'error') {
        useTransferErrorStore.getState().report(moveFailureFromEvent(event));
        return;
      }

      // If task doesn't exist in store (new task started from queue), reload from database
      if (
        !previous &&
        (newStatus === 'downloading' ||
          newStatus === 'uploading' ||
          isMoveAwaitingAction(newStatus))
      ) {
        // Reload all active tasks to get the new task
        invoke<MoveSession[]>('get_all_active_move_tasks')
          .then((sessions) => {
            useMoveStore.getState().loadFromDatabase(sessions);
          })
          .catch((e) => console.error('Failed to reload tasks:', e));
        return;
      }

      set((state) => ({
        tasks: state.tasks.map((t) => {
          if (t.id !== event.task_id) return t;

          const isTerminal = t.status === 'success' || t.status === 'cancelled';
          if (isTerminal && newStatus !== t.status) {
            return t;
          }

          if (t.status === 'error' && newStatus !== 'pending' && newStatus !== 'cancelled') {
            return t;
          }

          if (
            (t.status === 'finishing' || t.status === 'deleting') &&
            (newStatus === 'uploading' || newStatus === 'downloading')
          ) {
            return t;
          }

          return {
            ...t,
            status: newStatus,
            error: event.error || undefined,
            // Reset speed when task finishes, keep when active
            speed: newStatus === 'downloading' || newStatus === 'uploading' ? t.speed : 0,
            // Reset progress to 0 when task starts (pending → downloading)
            progress: t.status === 'pending' && newStatus === 'downloading' ? 0 : t.progress,
            phase: derivePhase(newStatus, t.phase),
          };
        }),
      }));

      const next = get().tasks.find((t) => t.id === event.task_id);
      reportFailureStep(previous, next);
      forgetFailureStep(previous, next);
    },

    handleTaskDeleted: (event) => {
      set((state) => ({
        tasks: state.tasks.filter((t) => t.id !== event.task_id),
      }));
    },
  };
});

export const selectPendingCount = (state: MoveStore) =>
  state.tasks.filter((t) => t.status === 'pending').length;

export const selectDownloadingCount = (state: MoveStore) =>
  state.tasks.filter((t) => t.status === 'downloading').length;

export const selectUploadingCount = (state: MoveStore) =>
  state.tasks.filter((t) => t.status === 'uploading' && t.progress < 100).length;

// Active transfers - tasks actively downloading/uploading (not at 100%, not post-sync)
export const selectActiveCount = (state: MoveStore) =>
  state.tasks.filter(
    (t) => t.status === 'downloading' || (t.status === 'uploading' && t.progress < 100) // Uploading but not finished
  ).length;

// Tasks in finishing phase (uploading at 100% or deleting) - shouldn't block new tasks
export const selectFinishingCount = (state: MoveStore) =>
  state.tasks.filter(
    (t) =>
      t.status === 'finishing' ||
      t.status === 'deleting' ||
      (t.status === 'uploading' && t.progress >= 100) // Upload complete, waiting for post-sync
  ).length;

// Deprecated: kept for compatibility, use selectFinishingCount instead
export const selectDeletingCount = selectFinishingCount;

export const selectPausedCount = (state: MoveStore) =>
  state.tasks.filter((t) => t.status === 'paused').length;

export const selectAttentionCount = (state: MoveStore) =>
  state.tasks.filter((task) => isMoveAwaitingAction(task.status)).length;

export const selectFinishedCount = (state: MoveStore) =>
  state.tasks.filter(
    (t) => t.status === 'success' || t.status === 'error' || t.status === 'cancelled'
  ).length;

// Has active transfers (downloading or uploading below 100%) - used for display
export const selectHasActiveMoves = (state: MoveStore) =>
  state.tasks.some(
    (t) => t.status === 'downloading' || (t.status === 'uploading' && t.progress < 100) // Uploading but not finished
  );

// Has any in-progress work (including finishing/deleting) - used for safety checks like "Clear All"
export const selectHasInProgressMoves = (state: MoveStore) =>
  state.tasks.some(
    (t) =>
      t.status === 'downloading' ||
      t.status === 'uploading' ||
      t.status === 'finishing' ||
      t.status === 'deleting'
  );

/**
 * Setup global move event listeners that persist across component unmounts.
 * Should be called once on app initialization.
 * These listeners will continue to receive updates even when components unmount.
 */
export async function setupGlobalMoveListeners(): Promise<void> {
  if (globalListenersSetup) return;
  globalListenersSetup = true;

  try {
    const unlistenProgress = await listen<MoveProgressEvent>('move-progress', (event) => {
      useMoveStore.getState().handleProgressEvent(event.payload);
    });
    globalUnlisteners.push(unlistenProgress);

    const unlistenStatus = await listen<MoveStatusChangedEvent>('move-status-changed', (event) => {
      useMoveStore.getState().handleStatusChanged(event.payload);
    });
    globalUnlisteners.push(unlistenStatus);

    const unlistenDeleted = await listen<MoveTaskDeletedEvent>('move-task-deleted', (event) => {
      useMoveStore.getState().handleTaskDeleted(event.payload);
    });
    globalUnlisteners.push(unlistenDeleted);

    // Reload all active tasks on batch operations (global, not filtered by account)
    const unlistenBatch = await listen<MoveBatchOperationEvent>(
      'move-batch-operation',
      async (event) => {
        const payload = event.payload;
        if (payload.operation === 'clear_all') {
          useMoveStore.setState((state) => ({
            tasks: state.tasks.filter(
              (task) =>
                !(
                  task.sourceBucket === payload.source_bucket &&
                  task.sourceAccountId === payload.source_account_id
                )
            ),
          }));
          return;
        } else if (payload.operation === 'clear_finished') {
          useMoveStore.setState((state) => ({
            tasks: state.tasks.filter((task) => {
              const isSourceMatch =
                task.sourceBucket === payload.source_bucket &&
                task.sourceAccountId === payload.source_account_id;
              return !(isSourceMatch && isFinishedStatus(task.status));
            }),
          }));
          return;
        }

        try {
          const sessions = await invoke<MoveSession[]>('get_all_active_move_tasks');
          useMoveStore.getState().loadFromDatabase(sessions);
        } catch (e) {
          console.error('Failed to reload move tasks after batch operation:', e);
        }
      }
    );
    globalUnlisteners.push(unlistenBatch);
  } catch (e) {
    console.error('Failed to setup global move listeners:', e);
    globalListenersSetup = false;
  }
}

/**
 * Load all active move tasks from database.
 * Call this to refresh the task list.
 */
export async function loadAllActiveMoves(): Promise<void> {
  try {
    const sessions = await invoke<MoveSession[]>('get_all_active_move_tasks');
    useMoveStore.getState().loadFromDatabase(sessions);
  } catch (e) {
    console.error('Failed to load all active move tasks:', e);
  }
}
