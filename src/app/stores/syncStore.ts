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

export interface BackgroundSyncState {
  isRunning: boolean;
  objectsFetched: number;
  estimatedTotal: number | null;
  startedAt: number | null;
  completedAt: number | null;
  speed: number; // objects/second
  error: string | null;
}

const initialBackgroundSync: BackgroundSyncState = {
  isRunning: false,
  objectsFetched: 0,
  estimatedTotal: null,
  startedAt: null,
  completedAt: null,
  speed: 0,
  error: null,
};

// Key format: "accountId:bucket"
function makeBucketKey(accountId: string, bucket: string): string {
  return `${accountId}:${bucket}`;
}

interface SyncStore {
  // High-level sync status
  isSyncing: boolean;
  isFolderLoading: boolean;

  // Per-bucket sync times: Map<"accountId:bucket", timestamp>
  bucketSyncTimes: Record<string, number>;
  // Current bucket key for convenience
  currentBucketKey: string | null;

  // Bucket sync state
  phase: SyncPhase;
  processedFiles: number;
  storedFiles: number;
  totalFiles: number;
  indexingProgress: IndexingProgress;

  // Actions - sync
  setIsSyncing: (syncing: boolean) => void;
  setLastSyncTime: (accountId: string, bucket: string, time: number | null) => void;
  getLastSyncTime: (accountId: string, bucket: string) => number | null;
  setCurrentBucket: (accountId: string | null, bucket: string | null) => void;
  setIsFolderLoading: (loading: boolean) => void;
  setPhase: (phase: SyncPhase) => void;
  setProgress: (count: number) => void;
  setStoredFiles: (count: number) => void;
  setTotalFiles: (count: number) => void;
  setIndexingProgress: (progress: IndexingProgress) => void;
  reset: () => void;
  resetProgress: () => void;

  // Folder loading state
  folderLoadPhase: FolderLoadPhase;
  folderLoadProgress: FolderLoadProgress;
  setFolderLoadPhase: (phase: FolderLoadPhase) => void;
  setFolderLoadProgress: (progress: FolderLoadProgress) => void;
  resetFolderLoad: () => void;

  // Background sync state
  backgroundSync: BackgroundSyncState;
  setBackgroundSyncProgress: (progress: Partial<BackgroundSyncState>) => void;
  startBackgroundSync: () => void;
  completeBackgroundSync: (totalObjects: number) => void;
  failBackgroundSync: (error: string) => void;
  resetBackgroundSync: () => void;
}

export const useSyncStore = create<SyncStore>((set, get) => ({
  // High-level sync status
  isSyncing: false,
  isFolderLoading: false,

  // Per-bucket sync times
  bucketSyncTimes: {},
  currentBucketKey: null,

  // Bucket sync state
  phase: 'idle',
  processedFiles: 0,
  storedFiles: 0,
  totalFiles: 0,
  indexingProgress: { current: 0, total: 0 },

  setIsSyncing: (syncing) => {
    set({ isSyncing: syncing });
  },

  setLastSyncTime: (accountId, bucket, time) => {
    const key = makeBucketKey(accountId, bucket);
    set((state) => {
      if (time === null) {
        // Remove the key from bucketSyncTimes
        const { [key]: _, ...rest } = state.bucketSyncTimes;
        return { bucketSyncTimes: rest };
      }
      return { bucketSyncTimes: { ...state.bucketSyncTimes, [key]: time } };
    });
  },

  getLastSyncTime: (accountId, bucket) => {
    const key = makeBucketKey(accountId, bucket);
    return get().bucketSyncTimes[key] ?? null;
  },

  setCurrentBucket: (accountId, bucket) => {
    const key = accountId && bucket ? makeBucketKey(accountId, bucket) : null;
    set({ currentBucketKey: key });
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

  setStoredFiles: (count) => {
    set({ storedFiles: count });
  },

  setTotalFiles: (count) => {
    set({ totalFiles: count });
  },

  setIndexingProgress: (progress) => {
    set({ indexingProgress: progress });
  },

  // Full reset including bucket sync times (for logout, etc.)
  reset: () => {
    set({
      phase: 'idle',
      processedFiles: 0,
      storedFiles: 0,
      totalFiles: 0,
      indexingProgress: { current: 0, total: 0 },
      bucketSyncTimes: {},
      currentBucketKey: null,
      backgroundSync: { ...initialBackgroundSync },
    });
  },

  // Reset only progress state (for refresh), preserves bucket sync times
  resetProgress: () => {
    set({
      phase: 'idle',
      processedFiles: 0,
      storedFiles: 0,
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

  // Background sync state
  backgroundSync: { ...initialBackgroundSync },

  startBackgroundSync: () => {
    set({
      backgroundSync: {
        isRunning: true,
        objectsFetched: 0,
        estimatedTotal: null,
        startedAt: Date.now(),
        completedAt: null,
        speed: 0,
        error: null,
      },
    });
  },

  setBackgroundSyncProgress: (progress) => {
    set((state) => ({
      backgroundSync: { ...state.backgroundSync, ...progress },
    }));
  },

  completeBackgroundSync: (totalObjects) => {
    set((state) => ({
      backgroundSync: {
        ...state.backgroundSync,
        isRunning: false,
        objectsFetched: totalObjects,
        estimatedTotal: totalObjects,
        completedAt: Date.now(),
      },
    }));
  },

  failBackgroundSync: (error) => {
    set((state) => ({
      backgroundSync: {
        ...state.backgroundSync,
        isRunning: false,
        error,
      },
    }));
  },

  resetBackgroundSync: () => {
    set({
      backgroundSync: { ...initialBackgroundSync },
    });
  },
}));
