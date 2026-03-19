import { useEffect, useMemo } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { getFolderContents, listPrefix, StoredFile } from '@/app/lib/r2cache';
import { StorageConfig } from '@/app/lib/r2cache';
import { useSyncStore } from '@/app/stores/syncStore';
import { useCurrentPathStore } from '@/app/stores/currentPathStore';

// Event emitted by backend when cache is updated
export interface FileItem {
  name: string;
  key: string;
  isFolder: boolean;
  size?: number;
  lastModified?: string;
}

interface MoveStatusChangedEvent {
  task_id: string;
  status: string;
  error: string | null;
}

const nameCollator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

function extractName(key: string, prefix: string): string {
  const relativePath = prefix ? key.slice(prefix.length) : key;
  return relativePath.replace(/\/$/, '');
}

function getParentPath(path: string): string {
  if (!path) return '';
  const withoutTrailing = path.endsWith('/') ? path.slice(0, -1) : path;
  const lastSlash = withoutTrailing.lastIndexOf('/');
  if (lastSlash === -1) return '';
  return `${withoutTrailing.slice(0, lastSlash + 1)}`;
}

function buildFileItems(files: StoredFile[], folders: string[], prefix: string): FileItem[] {
  const items: FileItem[] = [];

  // Add folders first (skip root "/" when at root level)
  for (const folder of folders) {
    if (folder === '/' || folder === '') continue;
    items.push({
      name: extractName(folder, prefix),
      key: folder,
      isFolder: true,
    });
  }

  // Add files
  for (const file of files) {
    if (file.key === prefix || file.key.endsWith('/')) continue;
    items.push({
      name: extractName(file.key, prefix),
      key: file.key,
      isFolder: false,
      size: file.size,
      lastModified: file.lastModified,
    });
  }

  return items.sort((a, b) => {
    if (a.isFolder && !b.isFolder) return -1;
    if (!a.isFolder && b.isFolder) return 1;
    return nameCollator.compare(a.name, b.name);
  });
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

      // Try local SQLite cache first (instant)
      const result = await getFolderContents(prefix);

      // If cache has data for this prefix, use it
      if (result.files.length > 0 || result.folders.length > 0) {
        return buildFileItems(result.files, result.folders, prefix);
      }

      // Cache empty — use lazy sync (ListObjectsV2 with delimiter)
      try {
        const lazyResult = await listPrefix(config, prefix);
        const storedFiles: StoredFile[] = lazyResult.files.map((f) => ({
          key: f.key,
          size: f.size,
          lastModified: f.last_modified,
        }));
        // Mark bucket as having data so other components can proceed
        useSyncStore.getState().setLastSyncTime(config.accountId, config.bucket, Date.now());
        return buildFileItems(storedFiles, lazyResult.folders, prefix);
      } catch (err) {
        // Fall back to empty if lazy sync fails (e.g. network error)
        console.error('[useR2Files] lazy sync failed for prefix:', prefix, err);
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
