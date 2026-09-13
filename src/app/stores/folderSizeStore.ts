import { create } from 'zustand';
import {
  calculateFolderSize,
  getDirectoryNode,
  getDirectoryNodes,
  DirectoryNode,
} from '@/app/lib/r2cache';

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
  generation: number;

  setSize: (key: string, size: FolderSizeState) => void;
  setMetadata: (key: string, data: FolderMetadata) => void;
  setMetadataBatch: (entries: [string, FolderMetadata][]) => void;

  calculateSize: (folderKey: string) => Promise<void>;
  calculateSizes: (folderKeys: string[]) => void;

  // New: Load metadata from directory tree
  loadMetadata: (folderKey: string) => Promise<void>;
  loadMetadataList: (folderKeys: string[]) => Promise<void>;

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
  generation: 0,

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

  setMetadataBatch: (entries) => {
    if (entries.length === 0) return;
    set((state) => {
      const metadata = { ...state.metadata };
      const sizes = { ...state.sizes };
      for (const [key, data] of entries) {
        metadata[key] = data;
        sizes[key] = data.size;
      }
      return { metadata, sizes };
    });
  },

  calculateSize: async (folderKey) => {
    const generation = get().generation;
    const { sizes, setSize } = get();

    // Skip if already calculated
    if (typeof sizes[folderKey] === 'number') return;

    setSize(folderKey, 'loading');

    try {
      const size = await calculateFolderSize(folderKey);
      if (get().generation !== generation) return;
      setSize(folderKey, size);
    } catch (err) {
      if (get().generation !== generation) return;
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
    const generation = get().generation;
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
      if (get().generation !== generation) return;
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
        if (get().generation !== generation) return;
        setMetadata(folderKey, {
          size,
          fileCount: null,
          totalFileCount: null,
          lastModified: null,
        });
      }
    } catch (err) {
      if (get().generation !== generation) return;
      console.error(`Failed to load metadata for ${folderKey}:`, err);
      setMetadata(folderKey, {
        size: 'error',
        fileCount: null,
        totalFileCount: null,
        lastModified: null,
      });
    }
  },

  loadMetadataList: async (folderKeys) => {
    const generation = get().generation;
    const { metadata, setMetadataBatch, loadMetadata } = get();

    // One IPC for the whole folder view instead of one per subfolder; a
    // single store update also means one re-sort of the file list rather
    // than one per resolved folder.
    const toLoad = folderKeys.filter((key) => {
      const existing = metadata[key];
      return !existing || !shouldReuseFolderMetadata(key, existing);
    });
    if (toLoad.length === 0) return;

    setMetadataBatch(
      toLoad.map((key) => [
        key,
        { size: 'loading' as const, fileCount: null, totalFileCount: null, lastModified: null },
      ])
    );

    try {
      const nodes = await getDirectoryNodes(toLoad);
      if (get().generation !== generation) return;
      const resolved: [string, FolderMetadata][] = [];
      const missing: string[] = [];

      toLoad.forEach((key, i) => {
        const node = nodes[i];
        if (node && !isProvisionalZeroNode(key, node)) {
          resolved.push([
            key,
            {
              size: node.totalSize,
              fileCount: node.fileCount,
              totalFileCount: node.totalFileCount,
              lastModified: node.lastModified,
            },
          ]);
        } else {
          missing.push(key);
        }
      });

      setMetadataBatch(resolved);

      // Folders the tree doesn't cover yet fall back to the per-folder scan
      // path (rare: only before background indexing has run).
      for (const key of missing) {
        loadMetadata(key);
      }
    } catch (err) {
      if (get().generation !== generation) return;
      console.error('Failed to batch-load folder metadata:', err);
      setMetadataBatch(
        toLoad.map((key) => [
          key,
          { size: 'error' as const, fileCount: null, totalFileCount: null, lastModified: null },
        ])
      );
    }
  },

  clearSizes: () => {
    set((state) => ({ sizes: {}, metadata: {}, generation: state.generation + 1 }));
  },
}));
