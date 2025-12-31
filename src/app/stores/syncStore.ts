import { create } from 'zustand';

export type SyncPhase = 'idle' | 'fetching' | 'storing' | 'indexing' | 'complete';
export type FolderLoadPhase = 'idle' | 'loading' | 'complete';

interface IndexingProgress {
  current: number;
  total: number;
}

interface FolderLoadProgress {
  pages: number;
  items: number;
}

interface SyncStore {
  // High-level sync status
  isSyncing: boolean;
  lastSyncTime: number | null;
  isFolderLoading: boolean;

  // Bucket sync state
  phase: SyncPhase;
  processedFiles: number;
  totalFiles: number;
  indexingProgress: IndexingProgress;

  // Actions - sync
  setIsSyncing: (syncing: boolean) => void;
  setLastSyncTime: (time: number | null) => void;
  setIsFolderLoading: (loading: boolean) => void;
  setPhase: (phase: SyncPhase) => void;
  setProgress: (count: number) => void;
  setTotalFiles: (count: number) => void;
  setIndexingProgress: (progress: IndexingProgress) => void;
  reset: () => void;

  // Folder loading state
  folderLoadPhase: FolderLoadPhase;
  folderLoadProgress: FolderLoadProgress;
  setFolderLoadPhase: (phase: FolderLoadPhase) => void;
  setFolderLoadProgress: (progress: FolderLoadProgress) => void;
  resetFolderLoad: () => void;
}

export const useSyncStore = create<SyncStore>((set) => ({
  // High-level sync status
  isSyncing: false,
  lastSyncTime: null,
  isFolderLoading: false,

  // Bucket sync state
  phase: 'idle',
  processedFiles: 0,
  totalFiles: 0,
  indexingProgress: { current: 0, total: 0 },

  setIsSyncing: (syncing) => {
    set({ isSyncing: syncing });
  },

  setLastSyncTime: (time) => {
    set({ lastSyncTime: time });
  },

  setIsFolderLoading: (loading) => {
    set({ isFolderLoading: loading });
  },

  setPhase: (phase) => {
    set({ phase });
  },

  setProgress: (count) => {
    set({ processedFiles: count });
  },

  setTotalFiles: (count) => {
    set({ totalFiles: count });
  },

  setIndexingProgress: (progress) => {
    set({ indexingProgress: progress });
  },

  reset: () => {
    set({
      phase: 'idle',
      processedFiles: 0,
      totalFiles: 0,
      indexingProgress: { current: 0, total: 0 },
    });
  },

  // Folder loading state
  folderLoadPhase: 'idle',
  folderLoadProgress: { pages: 0, items: 0 },

  setFolderLoadPhase: (phase) => {
    set({ folderLoadPhase: phase });
  },

  setFolderLoadProgress: (progress) => {
    set({ folderLoadProgress: progress });
  },

  resetFolderLoad: () => {
    set({
      folderLoadPhase: 'idle',
      folderLoadProgress: { pages: 0, items: 0 },
    });
  },
}));
