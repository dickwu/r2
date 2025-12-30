import { create } from 'zustand';

export type SyncPhase = 'idle' | 'fetching' | 'storing' | 'indexing' | 'complete';

interface SyncStore {
  phase: SyncPhase;
  processedFiles: number;
  totalFiles: number;
  setPhase: (phase: SyncPhase) => void;
  setProgress: (count: number) => void;
  setTotalFiles: (count: number) => void;
  reset: () => void;
}

export const useSyncStore = create<SyncStore>((set) => ({
  phase: 'idle',
  processedFiles: 0,
  totalFiles: 0,

  setPhase: (phase) => {
    set({ phase });
  },

  setProgress: (count) => {
    set({ processedFiles: count });
  },

  setTotalFiles: (count) => {
    set({ totalFiles: count });
  },

  reset: () => {
    set({ phase: 'idle', processedFiles: 0, totalFiles: 0 });
  },
}));
