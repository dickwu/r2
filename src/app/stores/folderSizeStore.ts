import { create } from 'zustand';
import { calculateFolderSize, getDirectoryNode, DirectoryNode } from '../lib/indexeddb';

type FolderSizeState = number | 'loading' | 'error';

export interface FolderMetadata {
  size: FolderSizeState;
  fileCount: number | null;
  totalFileCount: number | null;
}

interface FolderSizeStore {
  // Legacy single-value size map (for backward compatibility)
  sizes: Record<string, FolderSizeState>;
  // New metadata map with size and counts
  metadata: Record<string, FolderMetadata>;

  setSize: (key: string, size: FolderSizeState) => void;
  setMetadata: (key: string, data: FolderMetadata) => void;

  calculateSize: (folderKey: string) => Promise<void>;
  calculateSizes: (folderKeys: string[]) => void;

  // New: Load metadata from directory tree
  loadMetadata: (folderKey: string) => Promise<void>;
  loadMetadataList: (folderKeys: string[]) => void;

  clearSizes: () => void;
}

export const useFolderSizeStore = create<FolderSizeStore>((set, get) => ({
  sizes: {},
  metadata: {},

  setSize: (key, size) => {
    set((state) => ({
      sizes: { ...state.sizes, [key]: size },
    }));
  },

  setMetadata: (key, data) => {
    set((state) => ({
      metadata: { ...state.metadata, [key]: data },
      // Also update sizes for backward compatibility
      sizes: { ...state.sizes, [key]: data.size },
    }));
  },

  calculateSize: async (folderKey) => {
    const { sizes, setSize } = get();

    // Skip if already calculated
    if (typeof sizes[folderKey] === 'number') return;

    setSize(folderKey, 'loading');

    try {
      const size = await calculateFolderSize(folderKey);
      setSize(folderKey, size);
    } catch (err) {
      console.error(`Failed to calculate size for ${folderKey}:`, err);
      setSize(folderKey, 'error');
    }
  },

  calculateSizes: (folderKeys) => {
    const { calculateSize } = get();
    for (const key of folderKeys) {
      calculateSize(key);
    }
  },

  // Load metadata from pre-built directory tree
  loadMetadata: async (folderKey) => {
    const { metadata, setMetadata } = get();

    // Skip if already loaded
    if (metadata[folderKey] && typeof metadata[folderKey].size === 'number') return;

    // Set loading state
    setMetadata(folderKey, {
      size: 'loading',
      fileCount: null,
      totalFileCount: null,
    });

    try {
      const node = await getDirectoryNode(folderKey);
      if (node) {
        setMetadata(folderKey, {
          size: node.totalSize,
          fileCount: node.fileCount,
          totalFileCount: node.totalFileCount,
        });
      } else {
        // Fallback to old method if node not found
        const size = await calculateFolderSize(folderKey);
        setMetadata(folderKey, {
          size,
          fileCount: null,
          totalFileCount: null,
        });
      }
    } catch (err) {
      console.error(`Failed to load metadata for ${folderKey}:`, err);
      setMetadata(folderKey, {
        size: 'error',
        fileCount: null,
        totalFileCount: null,
      });
    }
  },

  loadMetadataList: (folderKeys) => {
    const { loadMetadata } = get();
    for (const key of folderKeys) {
      loadMetadata(key);
    }
  },

  clearSizes: () => {
    set({ sizes: {}, metadata: {} });
  },
}));
