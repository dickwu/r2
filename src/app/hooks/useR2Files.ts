import { useEffect } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { getFolderContents, StoredFile } from '../lib/r2cache';
import { R2Config } from '../components/ConfigModal';
import { useSyncStore } from '../stores/syncStore';

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

export function useR2Files(config: R2Config | null, prefix: string = '') {
  const queryClient = useQueryClient();
  const queryKey = ['folder-contents', config?.bucket, prefix];

  // Get sync status from store - only load from cache after sync completes
  const lastSyncTime = useSyncStore((state) => state.lastSyncTime);

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
    enabled: !!config?.token && !!config?.bucket && !!config?.accountId && lastSyncTime !== null,
    retry: 1,
  });

  // Sync isFetching state to zustand store as isFolderLoading
  useEffect(() => {
    useSyncStore.getState().setIsFolderLoading(query.isFetching);
  }, [query.isFetching]);

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
