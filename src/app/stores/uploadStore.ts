import { create } from 'zustand';
import type { StorageConfig } from '@/app/lib/r2cache';
import {
  DEFAULT_MAX_CONCURRENT_UPLOADS,
  clampUploadConcurrency,
  planUploadStarts,
} from '@/app/lib/uploadQueue';

// 'queued': the user started it, waiting for one of the concurrent upload slots
export type UploadStatus = 'pending' | 'queued' | 'uploading' | 'success' | 'error' | 'cancelled';

/** The backend is waiting to retry a failed request of this upload. */
export interface UploadRetryState {
  attempt: number; // the attempt about to be made, counting the first
  maxAttempts: number;
  error: string;
}

export interface UploadTask {
  id: string;
  filePath: string; // Native file path for Rust upload
  fileName: string;
  renamedFileName?: string; // Auto-renamed filename for collision avoidance
  fileSize: number;
  contentType: string;
  status: UploadStatus;
  progress: number;
  speed: number;
  error?: string;
  retry?: UploadRetryState;
}

/**
 * Runs one upload to completion and records its outcome on the task (success,
 * error or cancelled). Installed by `uploadRunner`, which owns the Tauri side;
 * the store only decides when each queued task starts.
 */
export type UploadRunner = (task: UploadTask) => Promise<void>;
export type UploadCanceler = (taskId: string) => void;

let runner: UploadRunner | null = null;
let canceler: UploadCanceler | null = null;

interface UploadStore {
  tasks: UploadTask[];
  uploadPath: string;
  config: StorageConfig | null;
  maxConcurrent: number;

  // Actions
  setConfig: (config: StorageConfig | null) => void;
  setUploadPath: (path: string) => void;
  setMaxConcurrent: (value: number) => void;
  setUploadRunner: (run: UploadRunner | null, cancel?: UploadCanceler | null) => void;
  addTasks: (tasks: Omit<UploadTask, 'id' | 'status' | 'progress' | 'speed'>[]) => void;
  removeTask: (id: string) => void;
  cancelTask: (id: string) => void;
  updateTask: (id: string, updates: Partial<UploadTask>) => void;
  startAllPending: () => void;
  pumpQueue: () => void;
  clearFinished: () => void;
  clearAll: () => void;
}

// Generate unique ID
let idCounter = 0;
function generateId(): string {
  return `upload-${Date.now()}-${++idCounter}`;
}

export const useUploadStore = create<UploadStore>((set, get) => ({
  tasks: [],
  uploadPath: '',
  config: null,
  maxConcurrent: DEFAULT_MAX_CONCURRENT_UPLOADS,

  setConfig: (config) => set({ config }),

  setUploadPath: (path) => set({ uploadPath: path }),

  setMaxConcurrent: (value) => {
    set({ maxConcurrent: clampUploadConcurrency(value) });
    // A higher limit starts more queued uploads now; a lower one lets the
    // running ones finish before the next starts.
    get().pumpQueue();
  },

  setUploadRunner: (run, cancel = null) => {
    runner = run;
    canceler = cancel;
    get().pumpQueue();
  },

  addTasks: (tasks) => {
    const newTasks: UploadTask[] = tasks.map((task) => ({
      ...task,
      id: generateId(),
      status: 'pending',
      progress: 0,
      speed: 0,
    }));
    set((state) => ({ tasks: [...state.tasks, ...newTasks] }));
  },

  removeTask: (id) => {
    const task = get().tasks.find((t) => t.id === id);
    if (task?.status === 'uploading') canceler?.(id);
    set((state) => ({ tasks: state.tasks.filter((t) => t.id !== id) }));
  },

  cancelTask: (id) => {
    const task = get().tasks.find((t) => t.id === id);
    if (!task) return;
    if (task.status === 'uploading') {
      // The runner records 'cancelled' once the backend stops
      canceler?.(id);
    } else if (task.status === 'queued' || task.status === 'pending') {
      get().updateTask(id, { status: 'cancelled', speed: 0, retry: undefined });
    }
  },

  updateTask: (id, updates) => {
    set((state) => ({
      tasks: state.tasks.map((t) => (t.id === id ? { ...t, ...updates } : t)),
    }));
  },

  startAllPending: () => {
    set((state) => ({
      tasks: state.tasks.map((t) =>
        t.status === 'pending' ? { ...t, status: 'queued' as const } : t
      ),
    }));
    get().pumpQueue();
  },

  pumpQueue: () => {
    const run = runner;
    if (!run) return;
    const { tasks, maxConcurrent } = get();
    const starts = planUploadStarts(tasks, maxConcurrent);
    if (starts.length === 0) return;

    const ids = new Set(starts.map((t) => t.id));
    set((state) => ({
      tasks: state.tasks.map((t) =>
        ids.has(t.id)
          ? { ...t, status: 'uploading' as const, progress: 0, speed: 0, retry: undefined }
          : t
      ),
    }));

    for (const { id } of starts) {
      const task = get().tasks.find((t) => t.id === id);
      if (!task) continue;
      Promise.resolve()
        .then(() => run(task))
        .catch((e: unknown) => {
          get().updateTask(id, {
            status: 'error',
            error: e instanceof Error ? e.message : String(e),
            speed: 0,
          });
        })
        .finally(() => {
          // A runner that settled without recording an outcome must not hold
          // its slot forever
          const current = get().tasks.find((t) => t.id === id);
          if (current?.status === 'uploading') {
            get().updateTask(id, {
              status: 'error',
              error: current.error || 'Upload ended unexpectedly',
              speed: 0,
            });
          }
          get().pumpQueue();
        });
    }
  },

  clearFinished: () => {
    set((state) => ({
      tasks: state.tasks.filter(
        (t) => t.status === 'pending' || t.status === 'queued' || t.status === 'uploading'
      ),
    }));
  },

  clearAll: () => {
    for (const t of get().tasks) {
      if (t.status === 'uploading') canceler?.(t.id);
    }
    set({ tasks: [] });
  },
}));

// Selectors
export const selectPendingCount = (state: UploadStore) =>
  state.tasks.filter((t) => t.status === 'pending').length;

export const selectQueuedCount = (state: UploadStore) =>
  state.tasks.filter((t) => t.status === 'queued').length;

export const selectUploadingCount = (state: UploadStore) =>
  state.tasks.filter((t) => t.status === 'uploading').length;

export const selectFinishedCount = (state: UploadStore) =>
  state.tasks.filter(
    (t) => t.status === 'success' || t.status === 'error' || t.status === 'cancelled'
  ).length;

export const selectHasActiveUploads = (state: UploadStore) =>
  state.tasks.some((t) => t.status === 'uploading' || t.status === 'queued');

export const selectHasSuccessfulUploads = (state: UploadStore) =>
  state.tasks.some((t) => t.status === 'success');
