import { create } from 'zustand';
import type { R2Config } from '../components/ConfigModal';

export type UploadStatus = 'pending' | 'uploading' | 'success' | 'error' | 'cancelled';

export interface UploadTask {
  id: string;
  filePath: string; // Native file path for Rust upload
  fileName: string;
  fileSize: number;
  contentType: string;
  status: UploadStatus;
  progress: number;
  speed: number;
  error?: string;
}

interface UploadStore {
  tasks: UploadTask[];
  uploadPath: string;
  config: R2Config | null;

  // Actions
  setConfig: (config: R2Config | null) => void;
  setUploadPath: (path: string) => void;
  addTasks: (tasks: Omit<UploadTask, 'id' | 'status' | 'progress' | 'speed'>[]) => void;
  removeTask: (id: string) => void;
  updateTask: (id: string, updates: Partial<UploadTask>) => void;
  startAllPending: () => void;
  clearFinished: () => void;
  clearAll: () => void;
}

// Generate unique ID
let idCounter = 0;
function generateId(): string {
  return `upload-${Date.now()}-${++idCounter}`;
}

export const useUploadStore = create<UploadStore>((set) => ({
  tasks: [],
  uploadPath: '',
  config: null,

  setConfig: (config) => set({ config }),

  setUploadPath: (path) => set({ uploadPath: path }),

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
    set((state) => ({ tasks: state.tasks.filter((t) => t.id !== id) }));
  },

  updateTask: (id, updates) => {
    set((state) => ({
      tasks: state.tasks.map((t) => (t.id === id ? { ...t, ...updates } : t)),
    }));
  },

  startAllPending: () => {
    set((state) => ({
      tasks: state.tasks.map((t) =>
        t.status === 'pending' ? { ...t, status: 'uploading' as const } : t
      ),
    }));
  },

  clearFinished: () => {
    set((state) => ({
      tasks: state.tasks.filter((t) => t.status === 'pending' || t.status === 'uploading'),
    }));
  },

  clearAll: () => {
    set({ tasks: [] });
  },
}));

// Selectors
export const selectPendingCount = (state: UploadStore) =>
  state.tasks.filter((t) => t.status === 'pending').length;

export const selectUploadingCount = (state: UploadStore) =>
  state.tasks.filter((t) => t.status === 'uploading').length;

export const selectFinishedCount = (state: UploadStore) =>
  state.tasks.filter(
    (t) => t.status === 'success' || t.status === 'error' || t.status === 'cancelled'
  ).length;

export const selectHasActiveUploads = (state: UploadStore) =>
  state.tasks.some((t) => t.status === 'uploading');




