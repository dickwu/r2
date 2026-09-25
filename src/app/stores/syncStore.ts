import { create } from 'zustand';
import type { TransferFailure } from '@/app/lib/transferFailure';
import { useTransferErrorStore } from '@/app/stores/transferErrorStore';

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
  bytesFetched: number;
  estimatedTotal: number | null;
  startedAt: number | null;
  completedAt: number | null;
  speed: number; // objects/second
  error: string | null;
}

const initialBackgroundSync: BackgroundSyncState = {
  isRunning: false,
  objectsFetched: 0,
  bytesFetched: 0,
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

// The account half may itself hold colons (useFilesSync keys accounts as
// "provider:account:namespace"); bucket names never do, so the last one splits.
function splitBucketKey(key: string | null): { accountId: string; bucket: string } | null {
  const at = key ? key.lastIndexOf(':') : -1;
  if (!key || at <= 0 || at === key.length - 1) return null;
  return { accountId: key.slice(0, at), bucket: key.slice(at + 1) };
}

/** The failure record id of a bucket's sync: one record per bucket, however often it fails. */
export function syncFailureId(currentBucketKey: string | null): string {
  const scope = splitBucketKey(currentBucketKey);
  return scope ? `sync:${scope.accountId}/${scope.bucket}` : 'sync:current';
}

/** What a sync failure record is built from: the bucket being synced and why it stopped. */
export interface SyncFailureSource {
  currentBucketKey: string | null;
  backgroundSync: Pick<BackgroundSyncState, 'error'>;
}

/**
 * The failure record for the current bucket's background sync, or null while
 * it has not failed. `failBackgroundSync` reports it and the sync pill's
 * "Show error" shows it, so both build the identical record. The id is per
 * bucket: a sync that keeps failing the same way keeps one record.
 */
export function syncFailureRecord(
  source: SyncFailureSource,
  now: number = Date.now()
): TransferFailure | null {
  const { error } = source.backgroundSync;
  if (!error) return null;
  const scope = splitBucketKey(source.currentBucketKey);
  return {
    id: syncFailureId(source.currentBucketKey),
    kind: 'sync',
    name: scope?.bucket ?? 'Bucket sync',
    message: error.trim() === '' ? 'Sync failed' : error,
    occurredAt: now,
    bucket: scope?.bucket,
  };
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
  completeBackgroundSync: (totalObjects: number, totalBytes?: number) => void;
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
        bytesFetched: 0,
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

  completeBackgroundSync: (totalObjects, totalBytes) => {
    set((state) => ({
      backgroundSync: {
        ...state.backgroundSync,
        isRunning: false,
        objectsFetched: totalObjects,
        bytesFetched: totalBytes ?? state.backgroundSync.bytesFetched,
        estimatedTotal: totalObjects,
        completedAt: Date.now(),
        // A run that finished supersedes whatever the last one failed with —
        // otherwise a late error from a cancelled run leaves the banner
        // reading "Sync failed" over a bucket that synced fine.
        error: null,
      },
    }));
    // The failure is over too: if the next run fails the same way, that is news.
    useTransferErrorStore.getState().forget(syncFailureId(get().currentBucketKey));
  },

  failBackgroundSync: (error) => {
    set((state) => ({
      backgroundSync: {
        ...state.backgroundSync,
        isRunning: false,
        error,
      },
    }));
    // The sync pill only says "Sync failed"; the reason goes to the failure modal.
    const failure = syncFailureRecord(get());
    if (failure) useTransferErrorStore.getState().report(failure);
  },

  resetBackgroundSync: () => {
    set({
      backgroundSync: { ...initialBackgroundSync },
    });
  },
}));
