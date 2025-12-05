import { create } from "zustand";
import { calculateFolderSize } from "../lib/indexeddb";

type FolderSizeState = number | "loading" | "error";

interface FolderSizeStore {
  sizes: Record<string, FolderSizeState>;
  setSize: (key: string, size: FolderSizeState) => void;
  calculateSize: (folderKey: string) => Promise<void>;
  calculateSizes: (folderKeys: string[]) => void;
  clearSizes: () => void;
}

export const useFolderSizeStore = create<FolderSizeStore>((set, get) => ({
  sizes: {},

  setSize: (key, size) => {
    set((state) => ({
      sizes: { ...state.sizes, [key]: size },
    }));
  },

  calculateSize: async (folderKey) => {
    const { sizes, setSize } = get();
    
    // Skip if already calculated
    if (typeof sizes[folderKey] === "number") return;
    
    setSize(folderKey, "loading");
    
    try {
      const size = await calculateFolderSize(folderKey);
      setSize(folderKey, size);
    } catch (err) {
      console.error(`Failed to calculate size for ${folderKey}:`, err);
      setSize(folderKey, "error");
    }
  },

  calculateSizes: (folderKeys) => {
    const { calculateSize } = get();
    for (const key of folderKeys) {
      calculateSize(key);
    }
  },

  clearSizes: () => {
    set({ sizes: {} });
  },
}));
