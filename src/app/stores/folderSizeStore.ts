import { create } from 'zustand';
import { calculateFolderSize, getDirectoryNode, DirectoryNode } from '@/app/lib/r2cache';

type FolderSizeState = number | 'loading' | 'error';

export interface FolderMetadata {
  size: FolderSizeState;
  fileCount: number | null;
  totalFileCount: number | null;
  lastModified: string | null;
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

export function isProvisionalZeroNode(path: string, node: DirectoryNode): boolean {
  return path !== '' && node.totalSize === 0 && node.totalFileCount === 0;
}

export function shouldReuseFolderMetadata(path: string, metadata: FolderMetadata): boolean {
  if (typeof metadata.size !== 'number') return false;

  // Fallback size calculations store counts as null. A zero byte fallback can
  // become stale after the folder is later lazy-loaded; a nonzero fallback can
  // still be partial if only some descendants are cached. Keep rechecking until
  // directory indexing gives us a real count.
  if (metadata.totalFileCount === null) return false;

  // Root is allowed to be a genuinely empty bucket. Non-root zero-count nodes
  // are lazy placeholders until directory indexing proves otherwise.
  return path === '' || metadata.size > 0 || metadata.totalFileCount !== 0;
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
    const existing = metadata[folderKey];

    if (existing && shouldReuseFolderMetadata(folderKey, existing)) {
      return;
    }

    // Set loading state
    setMetadata(folderKey, {
      size: 'loading',
      fileCount: null,
      totalFileCount: null,
      lastModified: null,
    });

    try {
      const node = await getDirectoryNode(folderKey);
      if (node && !isProvisionalZeroNode(folderKey, node)) {
        setMetadata(folderKey, {
          size: node.totalSize,
          fileCount: node.fileCount,
          totalFileCount: node.totalFileCount,
          lastModified: node.lastModified,
        });
      } else {
        // Fallback when the node is missing, or when lazy sync only created a
        // zero-valued placeholder before background indexing computed aggregates.
        const size = await calculateFolderSize(folderKey);
        setMetadata(folderKey, {
          size,
          fileCount: null,
          totalFileCount: null,
          lastModified: null,
        });
      }
    } catch (err) {
      console.error(`Failed to load metadata for ${folderKey}:`, err);
      setMetadata(folderKey, {
        size: 'error',
        fileCount: null,
        totalFileCount: null,
        lastModified: null,
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
