import { useEffect, useMemo } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { getAllFiles, getFolderContents, listPrefix } from '@/app/lib/r2cache';
import { StorageConfig } from '@/app/lib/r2cache';
import { useSyncStore } from '@/app/stores/syncStore';
import { useCurrentPathStore } from '@/app/stores/currentPathStore';
import { loadFolderItems } from '@/app/utils/folderItems';
import type { FileItem } from '@/app/utils/folderItems';

export type { FileItem } from '@/app/utils/folderItems';

// Event emitted by backend when cache is updated
interface MoveStatusChangedEvent {
  task_id: string;
  status: string;
  error: string | null;
}

function getParentPath(path: string): string {
  if (!path) return '';
  const withoutTrailing = path.endsWith('/') ? path.slice(0, -1) : path;
  const lastSlash = withoutTrailing.lastIndexOf('/');
  if (lastSlash === -1) return '';
  return `${withoutTrailing.slice(0, lastSlash + 1)}`;
}

export function useR2Files(config: StorageConfig | null, prefix: string = '') {
  const queryClient = useQueryClient();
  const queryKey = ['folder-contents', config?.provider, config?.accountId, config?.bucket, prefix];

  // Get per-bucket sync time - only load from cache after sync completes
  const bucketSyncTimes = useSyncStore((state) => state.bucketSyncTimes);
  const lastSyncTime = useMemo(() => {
    if (!config?.accountId || !config?.bucket) return null;
    return useSyncStore.getState().getLastSyncTime(config.accountId, config.bucket);
  }, [config?.accountId, config?.bucket, bucketSyncTimes]);

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

  const query = useQuery({
    queryKey,
    queryFn: async (): Promise<FileItem[]> => {
      if (!config) return [];

      try {
        const result = await loadFolderItems({
          config,
          prefix,
          forceRefresh: true,
          readCachedFolder: getFolderContents,
          readAllCachedFiles: getAllFiles,
          readPrefixFolder: listPrefix,
        });

        if (result.source === 'prefix') {
          useSyncStore.getState().setLastSyncTime(config.accountId, config.bucket, Date.now());
        }

        return result.items;
      } catch (err) {
        console.warn('[useR2Files] failed to load prefix and no cache fallback was available:', {
          prefix,
          err,
        });
        return [];
      }
    },
    // Enable immediately when config is ready — lazy sync handles missing cache
    enabled: isConfigReady,
    retry: 1,
  });

  // Sync isFetching state to zustand store as isFolderLoading
  useEffect(() => {
    useSyncStore.getState().setIsFolderLoading(query.isFetching);
  }, [query.isFetching]);

  const cacheUpdatedPaths = useCurrentPathStore((state) => state.cacheUpdatedPaths);
  const removedPaths = useCurrentPathStore((state) => state.removedPaths);
  const createdPaths = useCurrentPathStore((state) => state.createdPaths);

  // Auto-refresh affected folders when cache changes
  useEffect(() => {
    if (!config?.bucket) return;

    const invalidateFolderQueries = (paths: string[]) => {
      for (const path of paths) {
        queryClient.invalidateQueries({
          queryKey: ['folder-contents', config.provider, config.accountId, config.bucket, path],
        });
      }
    };

    const invalidateParentQueries = (paths: string[]) => {
      const parentPaths = new Set(paths.map(getParentPath));
      invalidateFolderQueries(Array.from(parentPaths));
    };
    if (cacheUpdatedPaths.length > 0) {
      invalidateFolderQueries(cacheUpdatedPaths);
    }
    if (removedPaths.length > 0) {
      invalidateParentQueries(removedPaths);
    }
    if (createdPaths.length > 0) {
      invalidateParentQueries(createdPaths);
    }
  }, [
    config?.bucket,
    config?.accountId,
    config?.provider,
    queryClient,
    cacheUpdatedPaths,
    removedPaths,
    createdPaths,
  ]);

  // Refresh current folder list after move completes
  useEffect(() => {
    if (!config?.bucket || !config?.accountId) return;

    let unlisten: UnlistenFn | undefined;

    const setup = async () => {
      unlisten = await listen<MoveStatusChangedEvent>('move-status-changed', (event) => {
        if (event.payload.status === 'success') {
          queryClient.invalidateQueries({
            queryKey: ['folder-contents', config.provider, config.accountId, config.bucket, prefix],
          });
        }
      });
    };

    setup();

    return () => {
      if (unlisten) unlisten();
    };
  }, [config?.bucket, config?.accountId, config?.provider, prefix, queryClient]);

  async function refresh() {
    await queryClient.invalidateQueries({ queryKey });
  }

  return {
    items: query.data ?? [],
    isLoading: query.isLoading,
    isFetching: query.isFetching,
    error: query.error,
    refresh,
  };
}
