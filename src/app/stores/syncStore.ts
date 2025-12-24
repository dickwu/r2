import { create } from 'zustand';

interface SyncStore {
  processedFiles: number;
  setProgress: (count: number) => void;
  reset: () => void;
}

export const useSyncStore = create<SyncStore>((set) => ({
  processedFiles: 0,

  setProgress: (count) => {
    set({ processedFiles: count });
  },

  reset: () => {
    set({ processedFiles: 0 });
  },
}));
