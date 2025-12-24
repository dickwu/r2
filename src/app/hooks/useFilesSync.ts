import { useCallback, useEffect } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { listen } from '@tauri-apps/api/event';
import {
  listAllR2ObjectsRecursive,
  storeAllFiles,
  buildDirectoryTree,
  getAllFiles,
} from '../lib/r2cache';
import { useFolderSizeStore } from '../stores/folderSizeStore';
import { useSyncStore } from '../stores/syncStore';
import { R2Config } from '../components/ConfigModal';

// Sync all files to IndexedDB for folder size calculation
export function useFilesSync(config: R2Config | null) {
  const queryClient = useQueryClient();
  const clearSizes = useFolderSizeStore((state) => state.clearSizes);

  // Listen for sync progress events from Tauri backend
  useEffect(() => {
    const unlisten = listen<number>('sync-progress', (event) => {
      useSyncStore.getState().setProgress(event.payload);
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const query = useQuery({
    queryKey: ['r2-all-files', config?.accountId, config?.bucket],
    queryFn: async () => {
      if (!config) return null;
      console.log('Syncing all files...');

      // Clear sizes and reset progress before resyncing
      clearSizes();
      useSyncStore.getState().reset();

      // Fetch all files from R2 (progress via Tauri events)
      const allFiles = await listAllR2ObjectsRecursive(config);

      // Store files in IndexedDB
      await storeAllFiles(allFiles);

      // Get stored files and build directory tree
      const storedFiles = await getAllFiles();
      await buildDirectoryTree(storedFiles);

      console.log(`Synced ${allFiles.length} files and built directory tree`);
      return {
        count: allFiles.length,
        timestamp: Date.now(),
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
    refresh,
  };
}
