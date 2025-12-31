import { create } from 'zustand';

export type SyncPhase = 'idle' | 'fetching' | 'storing' | 'indexing' | 'complete';

interface IndexingProgress {
  current: number;
  total: number;
}

interface SyncStore {
  phase: SyncPhase;
  processedFiles: number;
  totalFiles: number;
  indexingProgress: IndexingProgress;
  setPhase: (phase: SyncPhase) => void;
  setProgress: (count: number) => void;
  setTotalFiles: (count: number) => void;
  setIndexingProgress: (progress: IndexingProgress) => void;
  reset: () => void;
}

export const useSyncStore = create<SyncStore>((set) => ({
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
}));
