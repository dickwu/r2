import { useEffect, useMemo } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { getFolderContents, StoredFile } from '../lib/r2cache';
import { StorageConfig } from '../lib/r2cache';
import { useSyncStore } from '../stores/syncStore';

// Event emitted by backend when cache is updated
interface CacheUpdatedEvent {
  action: 'delete' | 'move' | 'update';
  affected_paths: string[];
}

export interface FileItem {
  name: string;
  key: string;
  isFolder: boolean;
  size?: number;
  lastModified?: string;
}

function extractName(key: string, prefix: string): string {
  const relativePath = prefix ? key.slice(prefix.length) : key;
  return relativePath.replace(/\/$/, '');
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
    return a.name.localeCompare(b.name);
  });
}

export function useR2Files(config: StorageConfig | null, prefix: string = '') {
  const queryClient = useQueryClient();
  const queryKey = [
    'folder-contents',
    config?.provider,
    config?.accountId,
    config?.bucket,
    prefix,
  ];

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
      console.log('Loading folder from cache:', { bucket: config.bucket, prefix });

      // Load from local SQLite cache (instant)
      const result = await getFolderContents(prefix);
      console.log('Cache result:', {
        files: result.files.length,
        folders: result.folders.length,
      });

      return buildFileItems(result.files, result.folders, prefix);
    },
    // Only enable after sync is complete (lastSyncTime is set)
    enabled: isConfigReady && lastSyncTime !== null,
    retry: 1,
  });

  // Sync isFetching state to zustand store as isFolderLoading
  useEffect(() => {
    useSyncStore.getState().setIsFolderLoading(query.isFetching);
  }, [query.isFetching]);

  // Listen for cache-updated events from backend to auto-refresh affected folders
  useEffect(() => {
    if (!config?.bucket) return;

    let unlisten: UnlistenFn | undefined;

    const setupListener = async () => {
      unlisten = await listen<CacheUpdatedEvent>('cache-updated', (event) => {
        const { affected_paths } = event.payload;
        console.log('Cache updated:', event.payload);

        // Invalidate queries for affected paths
        for (const path of affected_paths) {
          queryClient.invalidateQueries({
            queryKey: ['folder-contents', config.provider, config.accountId, config.bucket, path],
          });
        }
      });
    };

    setupListener();

    return () => {
      unlisten?.();
    };
  }, [config?.bucket, config?.accountId, config?.provider, queryClient]);

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
