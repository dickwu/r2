import { useCallback, useEffect, useMemo } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { listen } from '@tauri-apps/api/event';
import { listPrefix, StorageConfig } from '@/app/lib/r2cache';
import { useSyncStore } from '@/app/stores/syncStore';

/**
 * Foreground lazy sync hook.
 * Replaces useFilesSync for the primary browsing experience.
 * Calls list_prefix (delimiter="/") for each folder on navigation.
 */
export function useLazySync(config: StorageConfig | null) {
  const queryClient = useQueryClient();

  const isConfigReady = useMemo(() => {
    if (!config?.accountId || !config?.bucket) return false;
    if (config.provider === 'r2') {
      return !!config.accessKeyId && !!config.secretAccessKey;
    }
    if (config.provider === 'aws') {
      return !!config.accessKeyId && !!config.secretAccessKey && !!config.region;
    }
    return (
      !!config.accessKeyId &&
      !!config.secretAccessKey &&
      !!config.endpointHost &&
      !!config.endpointScheme
    );
  }, [config]);

  // Listen for folder-load-progress events (emitted by list_prefix for multi-page prefixes)
  useEffect(() => {
    const unlistenProgress = listen<{ pages: number; items: number }>(
      'folder-load-progress',
      (event) => {
        useSyncStore.getState().setFolderLoadProgress(event.payload);
      }
    );

    return () => {
      unlistenProgress.then((fn) => fn());
    };
  }, []);

  /**
   * Sync a specific prefix (folder).
   * Called by useR2Files when navigating to a folder.
   */
  const syncPrefix = useCallback(
    async (prefix: string) => {
      if (!config || !isConfigReady) return null;

      useSyncStore.getState().setFolderLoadPhase('loading');

      try {
        const result = await listPrefix(config, prefix);

        // Mark this bucket as "has data" (at least one prefix synced)
        useSyncStore.getState().setLastSyncTime(config.accountId, config.bucket, Date.now());

        useSyncStore.getState().setFolderLoadPhase('complete');
        useSyncStore.getState().resetFolderLoad();

        return result;
      } catch (error) {
        useSyncStore.getState().setFolderLoadPhase('idle');
        throw error;
      }
    },
    [config, isConfigReady]
  );

  const refresh = useCallback(
    async (prefix: string) => {
      if (!config) return;
      // Force re-fetch by invalidating the query for this prefix
      await queryClient.invalidateQueries({
        queryKey: ['folder-contents', config.provider, config.accountId, config.bucket, prefix],
      });
    },
    [queryClient, config]
  );

  return {
    syncPrefix,
    refresh,
    isConfigReady,
  };
}
