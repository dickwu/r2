import { useCallback, useEffect } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { listen } from '@tauri-apps/api/event';
import { syncBucket } from '../lib/r2cache';
import { useFolderSizeStore } from '../stores/folderSizeStore';
import { useSyncStore, SyncPhase } from '../stores/syncStore';
import { R2Config } from '../components/ConfigModal';

interface IndexingProgress {
  current: number;
  total: number;
}

// Sync all files to SQLite for folder size calculation
export function useFilesSync(config: R2Config | null) {
  const queryClient = useQueryClient();
  const clearSizes = useFolderSizeStore((state) => state.clearSizes);

  // Listen for sync progress and phase events from Tauri backend
  useEffect(() => {
    const unlistenProgress = listen<number>('sync-progress', (event) => {
      useSyncStore.getState().setProgress(event.payload);
    });

    // Listen for phase change events from backend
    const unlistenPhase = listen<SyncPhase>('sync-phase', (event) => {
      useSyncStore.getState().setPhase(event.payload);
    });

    // Listen for indexing progress events
    const unlistenIndexing = listen<IndexingProgress>('indexing-progress', (event) => {
      useSyncStore.getState().setIndexingProgress(event.payload);
    });

    return () => {
      unlistenProgress.then((fn) => fn());
      unlistenPhase.then((fn) => fn());
      unlistenIndexing.then((fn) => fn());
    };
  }, []);

  const query = useQuery({
    queryKey: ['r2-all-files', config?.accountId, config?.bucket],
    queryFn: async () => {
      if (!config) return null;
      console.log('Syncing bucket...');

      // Clear sizes and reset progress before resyncing
      clearSizes();
      useSyncStore.getState().reset();

      // Single backend call: fetch from R2, store in DB, build directory tree
      // Progress events are emitted via Tauri events (sync-progress, sync-phase, indexing-progress)
      const result = await syncBucket(config);

      console.log(`Synced ${result.count} files and built directory tree`);
      useSyncStore.getState().setTotalFiles(result.count);
      useSyncStore.getState().setLastSyncTime(result.timestamp);

      return {
        count: result.count,
        timestamp: result.timestamp,
        treeBuilt: true,
      };
    },
    enabled: !!config?.token && !!config?.bucket && !!config?.accountId,
    staleTime: 5 * 60 * 1000, // 5 minutes
    retry: 1,
    // Prevent concurrent syncs for the same bucket
    refetchInterval: false,
    refetchOnMount: false,
    refetchOnReconnect: false,
  });

  // Sync isSyncing state to zustand store
  useEffect(() => {
    useSyncStore.getState().setIsSyncing(query.isFetching);
  }, [query.isFetching]);

  const refresh = useCallback(async () => {
    // Prevent concurrent syncs - check if already syncing
    if (query.isFetching) {
      console.log('Sync already in progress, skipping...');
      return;
    }
    clearSizes();
    useSyncStore.getState().reset();
    await queryClient.invalidateQueries({
      queryKey: ['r2-all-files', config?.accountId, config?.bucket],
    });
  }, [queryClient, config?.accountId, config?.bucket, clearSizes, query.isFetching]);

  return {
    isSyncing: query.isFetching,
    isSynced: query.isSuccess,
    syncError: query.error,
    // Expose timestamp so consumers can detect when sync completes
    lastSyncTime: query.data?.timestamp ?? null,
    refresh,
  };
}
