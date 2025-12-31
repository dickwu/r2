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
  // Bucket sync state
  phase: SyncPhase;
  processedFiles: number;
  totalFiles: number;
  indexingProgress: IndexingProgress;
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
  // Bucket sync state
  phase: 'idle',
  processedFiles: 0,
  totalFiles: 0,
  indexingProgress: { current: 0, total: 0 },

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
