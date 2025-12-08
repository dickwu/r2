import { useCallback } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { listAllR2ObjectsRecursive } from '../lib/r2api';
import { storeAllFiles } from '../lib/indexeddb';
import { useFolderSizeStore } from '../stores/folderSizeStore';
import { R2Config } from '../components/ConfigModal';

// Sync all files to IndexedDB for folder size calculation
export function useFilesSync(config: R2Config | null) {
  const queryClient = useQueryClient();
  const clearSizes = useFolderSizeStore((state) => state.clearSizes);

  const query = useQuery({
    queryKey: ['r2-all-files', config?.bucket],
    queryFn: async () => {
      if (!config) return null;
      console.log('Syncing all files to IndexedDB...');

      const allFiles = await listAllR2ObjectsRecursive(config);
      await storeAllFiles(allFiles);

      console.log(`Synced ${allFiles.length} files to IndexedDB`);
      return { count: allFiles.length, timestamp: Date.now() };
    },
    enabled: !!config?.token && !!config?.bucket && !!config?.accountId,
    staleTime: 5 * 60 * 1000, // 5 minutes
    retry: 1,
  });

  const refresh = useCallback(async () => {
    clearSizes();
    await queryClient.invalidateQueries({ queryKey: ['r2-all-files', config?.bucket] });
  }, [queryClient, config?.bucket, clearSizes]);

  return {
    isSyncing: query.isFetching,
    isSynced: query.isSuccess,
    syncError: query.error,
    refresh,
  };
}

