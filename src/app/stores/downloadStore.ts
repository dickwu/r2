import { create } from 'zustand';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';

// Maximum concurrent downloads
export const MAX_CONCURRENT_DOWNLOADS = 5;

// Throttle progress updates to max once per 200ms per task
const PROGRESS_THROTTLE_MS = 200;
const progressThrottleMap = new Map<string, number>();
const pendingProgressUpdates = new Map<string, DownloadProgressEvent>();
let progressFlushTimer: ReturnType<typeof setTimeout> | null = null;

// Global listener state - persists across component unmounts
let globalListenersSetup = false;
let globalUnlisteners: UnlistenFn[] = [];

// Event types from Rust backend
export interface DownloadProgressEvent {
  task_id: string;
  percent: number;
  downloaded_bytes: number;
  total_bytes: number;
  speed: number;
}

export interface DownloadStatusChangedEvent {
  task_id: string;
  status: string;
  error: string | null;
}

export interface DownloadTaskDeletedEvent {
  task_id: string;
}

export interface DownloadBatchOperationEvent {
  operation: string; // "clear_finished" | "clear_all" | "pause_all" | "resume_all"
  bucket: string;
  account_id: string;
}

export type DownloadStatus =
  | 'pending'
  | 'downloading'
  | 'paused'
  | 'success'
  | 'error'
  | 'cancelled';

export interface DownloadTask {
  id: string;
  key: string; // R2 object key
  fileName: string;
  fileSize: number;
  localPath: string; // Destination path
  status: DownloadStatus;
  progress: number;
  downloadedBytes: number;
  speed: number;
  error?: string;
}

// Database session type (from Rust)
export interface DownloadSession {
  id: string;
  object_key: string;
  file_name: string;
  file_size: number;
  downloaded_bytes: number;
  local_path: string;
  bucket: string;
  account_id: string;
  status: string;
  error: string | null;
  created_at: number;
  updated_at: number;
}

interface DownloadStore {
  tasks: DownloadTask[];
  modalOpen: boolean;

  // Actions
  addTask: (task: Omit<DownloadTask, 'status' | 'progress' | 'downloadedBytes' | 'speed'>) => void;
  addTasks: (
    tasks: Omit<DownloadTask, 'status' | 'progress' | 'downloadedBytes' | 'speed'>[]
  ) => void;
  removeTask: (id: string) => void;
  updateTask: (id: string, updates: Partial<DownloadTask>) => void;
  clearFinished: () => void;
  clearAll: () => void;
  setModalOpen: (open: boolean) => void;
  loadFromDatabase: (sessions: DownloadSession[]) => void;

  // Event handlers (called by event listeners)
  handleProgressEvent: (event: DownloadProgressEvent) => void;
  handleStatusChanged: (event: DownloadStatusChangedEvent) => void;
  handleTaskDeleted: (event: DownloadTaskDeletedEvent) => void;

  // Queue management
  getTasksToStart: () => DownloadTask[];
  canStartMore: () => boolean;
}

// Convert database status to frontend status
function mapStatus(dbStatus: string): DownloadStatus {
  switch (dbStatus) {
    case 'pending':
      return 'pending';
    case 'downloading':
      return 'downloading';
    case 'paused':
      return 'paused';
    case 'completed':
      return 'success';
    case 'failed':
      return 'error';
    case 'cancelled':
      return 'cancelled';
    default:
      return 'pending';
  }
}

export const useDownloadStore = create<DownloadStore>((set, get) => ({
  tasks: [],
  modalOpen: false,

  addTask: (task) => {
    const newTask: DownloadTask = {
      ...task,
      status: 'pending',
      progress: 0,
      downloadedBytes: 0,
      speed: 0,
    };
    set((state) => ({ tasks: [...state.tasks, newTask] }));
  },

  addTasks: (tasks) => {
    const newTasks: DownloadTask[] = tasks.map((task) => ({
      ...task,
      status: 'pending',
      progress: 0,
      downloadedBytes: 0,
      speed: 0,
    }));
    set((state) => ({ tasks: [...state.tasks, ...newTasks] }));
  },

  removeTask: (id) => {
    set((state) => ({ tasks: state.tasks.filter((t) => t.id !== id) }));
  },

  updateTask: (id, updates) => {
    set((state) => ({
      tasks: state.tasks.map((t) => (t.id === id ? { ...t, ...updates } : t)),
    }));
  },

  clearFinished: () => {
    set((state) => ({
      tasks: state.tasks.filter(
        (t) => t.status === 'pending' || t.status === 'downloading' || t.status === 'paused'
      ),
    }));
  },

  clearAll: () => {
    set({ tasks: [] });
  },

  setModalOpen: (open) => {
    set({ modalOpen: open });
  },

  loadFromDatabase: (sessions) => {
    const currentTasks = get().tasks;
    const tasks: DownloadTask[] = sessions.map((session) => {
      // Find existing task to preserve real-time data (progress, speed)
      const existing = currentTasks.find((t) => t.id === session.id);
      const dbStatus = mapStatus(session.status);

      // If task is actively downloading, preserve real-time progress/speed from events
      const isActiveDownload = existing?.status === 'downloading' || dbStatus === 'downloading';

      // Get values from database with fallbacks
      const dbDownloadedBytes = session.downloaded_bytes || 0;
      const dbFileSize = session.file_size || 0;

      // Calculate progress from database values
      const dbProgress =
        dbFileSize > 0 ? Math.min(Math.round((dbDownloadedBytes / dbFileSize) * 100), 100) : 0;

      return {
        id: session.id,
        key: session.object_key,
        fileName: session.file_name,
        fileSize: dbFileSize,
        localPath: session.local_path,
        status: dbStatus,
        // For active downloads, use existing real-time data; otherwise use DB values
        progress: isActiveDownload && existing ? existing.progress : dbProgress,
        downloadedBytes:
          isActiveDownload && existing ? existing.downloadedBytes : dbDownloadedBytes,
        speed: isActiveDownload && existing ? existing.speed : 0,
        error: session.error || undefined,
      };
    });
    set({ tasks });
  },

  // Event handler for progress updates - throttled to prevent excessive re-renders
  handleProgressEvent: (event) => {
    const now = Date.now();
    const lastUpdate = progressThrottleMap.get(event.task_id) || 0;

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
              const newProgress = Math.max(t.progress, pendingEvent.percent);
              const newDownloadedBytes = Math.max(t.downloadedBytes, pendingEvent.downloaded_bytes);
              const newFileSize = t.fileSize > 0 ? t.fileSize : pendingEvent.total_bytes;

              return {
                ...t,
                progress: newProgress,
                downloadedBytes: newDownloadedBytes,
                fileSize: newFileSize,
                speed: pendingEvent.speed,
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

        const newProgress = Math.max(t.progress, event.percent);
        const newDownloadedBytes = Math.max(t.downloadedBytes, event.downloaded_bytes);
        const newFileSize = t.fileSize > 0 ? t.fileSize : event.total_bytes;

        return {
          ...t,
          progress: newProgress,
          downloadedBytes: newDownloadedBytes,
          fileSize: newFileSize,
          speed: event.speed,
        };
      }),
    }));
  },

  // Event handler for status changes
  handleStatusChanged: (event) => {
    const newStatus = mapStatus(event.status);
    set((state) => ({
      tasks: state.tasks.map((t) =>
        t.id === event.task_id
          ? {
              ...t,
              status: newStatus,
              error: event.error || undefined,
              // Reset speed when not downloading
              speed: newStatus === 'downloading' ? t.speed : 0,
            }
          : t
      ),
    }));
  },

  // Event handler for task deletion
  handleTaskDeleted: (event) => {
    set((state) => ({
      tasks: state.tasks.filter((t) => t.id !== event.task_id),
    }));
  },

  // Get tasks that should be started (up to MAX_CONCURRENT_DOWNLOADS)
  getTasksToStart: () => {
    const { tasks } = get();
    const activeCount = tasks.filter((t) => t.status === 'downloading').length;
    const slotsAvailable = MAX_CONCURRENT_DOWNLOADS - activeCount;

    if (slotsAvailable <= 0) return [];

    // Get pending tasks (not paused - those need manual resume)
    const pendingTasks = tasks.filter((t) => t.status === 'pending');
    return pendingTasks.slice(0, slotsAvailable);
  },

  // Check if we can start more downloads
  canStartMore: () => {
    const { tasks } = get();
    const activeCount = tasks.filter((t) => t.status === 'downloading').length;
    return activeCount < MAX_CONCURRENT_DOWNLOADS;
  },
}));

// Selectors
export const selectPendingCount = (state: DownloadStore) =>
  state.tasks.filter((t) => t.status === 'pending').length;

export const selectDownloadingCount = (state: DownloadStore) =>
  state.tasks.filter((t) => t.status === 'downloading').length;

export const selectPausedCount = (state: DownloadStore) =>
  state.tasks.filter((t) => t.status === 'paused').length;

export const selectFinishedCount = (state: DownloadStore) =>
  state.tasks.filter(
    (t) => t.status === 'success' || t.status === 'error' || t.status === 'cancelled'
  ).length;

export const selectHasActiveDownloads = (state: DownloadStore) =>
  state.tasks.some((t) => t.status === 'downloading');

export const selectTotalProgress = (state: DownloadStore) => {
  const activeTasks = state.tasks.filter(
    (t) => t.status === 'downloading' || t.status === 'success'
  );
  if (activeTasks.length === 0) return 0;

  const totalBytes = activeTasks.reduce((sum, t) => sum + t.fileSize, 0);
  const downloadedBytes = activeTasks.reduce((sum, t) => sum + t.downloadedBytes, 0);

  return totalBytes > 0 ? Math.round((downloadedBytes / totalBytes) * 100) : 0;
};

// Selector to check if we can start more downloads
export const selectCanStartMore = (state: DownloadStore) => {
  const activeCount = state.tasks.filter((t) => t.status === 'downloading').length;
  return activeCount < MAX_CONCURRENT_DOWNLOADS;
};

/**
 * Setup global download event listeners that persist across component unmounts.
 * Should be called once on app initialization.
 */
export async function setupGlobalDownloadListeners(): Promise<void> {
  if (globalListenersSetup) return;
  globalListenersSetup = true;

  try {
    // Listen for progress events
    const unlistenProgress = await listen<DownloadProgressEvent>('download-progress', (event) => {
      useDownloadStore.getState().handleProgressEvent(event.payload);
    });
    globalUnlisteners.push(unlistenProgress);

    // Listen for status change events
    const unlistenStatus = await listen<DownloadStatusChangedEvent>(
      'download-status-changed',
      (event) => {
        useDownloadStore.getState().handleStatusChanged(event.payload);
      }
    );
    globalUnlisteners.push(unlistenStatus);

    // Listen for task deleted events
    const unlistenDeleted = await listen<DownloadTaskDeletedEvent>(
      'download-task-deleted',
      (event) => {
        useDownloadStore.getState().handleTaskDeleted(event.payload);
      }
    );
    globalUnlisteners.push(unlistenDeleted);

    // Listen for batch operation events - reload from database
    const unlistenBatch = await listen<DownloadBatchOperationEvent>(
      'download-batch-operation',
      async () => {
        // Reload tasks (the component will filter by current bucket)
        // This is a simple approach - just reload whatever tasks exist
        try {
          // We don't filter here - let the UI components handle filtering
        } catch (e) {
          console.error('Failed to handle download batch operation:', e);
        }
      }
    );
    globalUnlisteners.push(unlistenBatch);
  } catch (e) {
    console.error('Failed to setup global download listeners:', e);
    globalListenersSetup = false;
  }
}

/**
 * Load download tasks for a specific bucket.
 */
export async function loadDownloadTasks(bucket: string, accountId: string): Promise<void> {
  try {
    const sessions = await invoke<DownloadSession[]>('get_download_tasks', {
      bucket,
      accountId,
    });
    useDownloadStore.getState().loadFromDatabase(sessions);
  } catch (e) {
    console.error('Failed to load download tasks:', e);
  }
}
