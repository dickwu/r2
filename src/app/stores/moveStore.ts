import { create } from 'zustand';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';

export const MAX_CONCURRENT_MOVES = 5;

// Throttle progress updates to max once per 200ms per task
const PROGRESS_THROTTLE_MS = 200;
const progressThrottleMap = new Map<string, number>();
const pendingProgressUpdates = new Map<string, MoveProgressEvent>();
let progressFlushTimer: ReturnType<typeof setTimeout> | null = null;

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

export const useMoveStore = create<MoveStore>((set, get) => ({
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
  },

  handleProgressEvent: (event) => {
    const now = Date.now();
    const lastUpdate = progressThrottleMap.get(event.task_id) || 0;

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

    // Store the latest event for this task
    pendingProgressUpdates.set(event.task_id, event);

    // If within throttle window, schedule a flush instead of immediate update
    if (now - lastUpdate < PROGRESS_THROTTLE_MS) {
      if (!progressFlushTimer) {
        progressFlushTimer = setTimeout(() => {
          progressFlushTimer = null;
          const updates = new Map(pendingProgressUpdates);
          pendingProgressUpdates.clear();

          if (updates.size === 0) return;

          set((state) => ({
            tasks: state.tasks.map((t) => {
              const pendingEvent = updates.get(t.id);
              if (!pendingEvent) return t;

              progressThrottleMap.set(t.id, Date.now());
              // Only use Math.max if task is already active, not if just starting
              const isActive = t.status === 'downloading' || t.status === 'uploading';
              const newProgress = isActive ? Math.max(t.progress, pendingEvent.percent) : pendingEvent.percent;
              const newTransferred = isActive
                ? Math.max(t.transferredBytes, pendingEvent.transferred_bytes)
                : pendingEvent.transferred_bytes;
              const newFileSize = t.fileSize > 0 ? t.fileSize : pendingEvent.total_bytes;
              return {
                ...t,
                progress: newProgress,
                transferredBytes: newTransferred,
                fileSize: newFileSize,
                speed: pendingEvent.speed,
                phase: pendingEvent.phase || t.phase,
                // Update status to match phase if task was pending
                status: t.status === 'pending' && pendingEvent.phase ? mapStatus(pendingEvent.phase) : t.status,
              };
            }),
          }));
        }, PROGRESS_THROTTLE_MS);
      }
      return;
    }

    // Immediate update (first update or outside throttle window)
    progressThrottleMap.set(event.task_id, now);
    pendingProgressUpdates.delete(event.task_id);

    set((state) => ({
      tasks: state.tasks.map((t) => {
        if (t.id !== event.task_id) return t;
        // Only use Math.max if task is already active, not if just starting
        const isActive = t.status === 'downloading' || t.status === 'uploading';
        const newProgress = isActive ? Math.max(t.progress, event.percent) : event.percent;
        const newTransferred = isActive
          ? Math.max(t.transferredBytes, event.transferred_bytes)
          : event.transferred_bytes;
        const newFileSize = t.fileSize > 0 ? t.fileSize : event.total_bytes;
        return {
          ...t,
          progress: newProgress,
          transferredBytes: newTransferred,
          fileSize: newFileSize,
          speed: event.speed,
          phase: event.phase || t.phase,
          // Update status to match phase if task was pending
          status: t.status === 'pending' && event.phase ? mapStatus(event.phase) : t.status,
        };
      }),
    }));
  },

  handleStatusChanged: (event) => {
    const newStatus = mapStatus(event.status);
    const taskExists = useMoveStore.getState().tasks.some((t) => t.id === event.task_id);

    // If task doesn't exist in store (new task started from queue), reload from database
    if (!taskExists && (newStatus === 'downloading' || newStatus === 'uploading')) {
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
  },

  handleTaskDeleted: (event) => {
    set((state) => ({
      tasks: state.tasks.filter((t) => t.id !== event.task_id),
    }));
  },
}));

export const selectPendingCount = (state: MoveStore) =>
  state.tasks.filter((t) => t.status === 'pending').length;

export const selectDownloadingCount = (state: MoveStore) =>
  state.tasks.filter((t) => t.status === 'downloading').length;

export const selectUploadingCount = (state: MoveStore) =>
  state.tasks.filter((t) => t.status === 'uploading' && t.progress < 100).length;

// Active transfers - tasks actively downloading/uploading (not at 100%, not post-sync)
export const selectActiveCount = (state: MoveStore) =>
  state.tasks.filter(
    (t) =>
      t.status === 'downloading' ||
      (t.status === 'uploading' && t.progress < 100) // Uploading but not finished
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

export const selectFinishedCount = (state: MoveStore) =>
  state.tasks.filter((t) => t.status === 'success' || t.status === 'error' || t.status === 'cancelled')
    .length;

// Has active transfers (downloading or uploading below 100%) - used for display
export const selectHasActiveMoves = (state: MoveStore) =>
  state.tasks.some(
    (t) =>
      t.status === 'downloading' ||
      (t.status === 'uploading' && t.progress < 100) // Uploading but not finished
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
    const unlistenBatch = await listen<MoveBatchOperationEvent>('move-batch-operation', async () => {
      try {
        const sessions = await invoke<MoveSession[]>('get_all_active_move_tasks');
        useMoveStore.getState().loadFromDatabase(sessions);
      } catch (e) {
        console.error('Failed to reload move tasks after batch operation:', e);
      }
    });
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
