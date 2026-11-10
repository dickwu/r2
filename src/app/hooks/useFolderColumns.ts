import { useQueries, useQueryClient } from '@tanstack/react-query';
import {
  getPrefixCache,
  hasSigningCredentials,
  streamFolderPrefix,
  type StorageConfig,
} from '@/app/lib/r2cache';
import { useStorageNamespace } from '@/app/hooks/useStorageNamespace';
import { loadFolderItems, type FileItem, type FolderSnapshot } from '@/app/utils/folderItems';
import { sortColumnItems } from '@/app/utils/columnView';

export interface FolderColumn {
  prefix: string;
  items: FileItem[];
  isLoading: boolean;
  error: Error | null;
}

/**
 * Listings for the ancestor columns of the column view, one query per folder
 * prefix. The last column (the current folder) comes from the page's own
 * useR2Files listing, so the toolbar's refresh keeps working on it unchanged.
 *
 * The query key matches useR2Files exactly, so these columns reuse any
 * listing fetched while browsing, and the cache-update / move invalidation in
 * useR2Files refreshes them too. Unlike useR2Files this hook has no side
 * effects of its own (no global loading flag, no query eviction), so it can
 * mount and unmount with the view without disturbing the list view.
 */
export function useFolderColumns(config: StorageConfig | null, prefixes: string[]): FolderColumn[] {
  const queryClient = useQueryClient();
  const namespace = useStorageNamespace(config);
  const enabled = hasSigningCredentials(config) && namespace !== null;

  const results = useQueries({
    queries: prefixes.map((prefix) => {
      const queryKey = [
        'folder-contents',
        config?.provider,
        config?.accountId,
        config?.bucket,
        prefix,
        namespace,
      ];
      return {
        queryKey,
        queryFn: async ({ signal }: { signal: AbortSignal }): Promise<FolderSnapshot> =>
          loadFolderItems({
            config: config as StorageConfig,
            prefix,
            signal,
            forceRefresh: false,
            fallbackSnapshot: queryClient.getQueryData<FolderSnapshot>(queryKey),
            readCachedFolder: getPrefixCache,
            readPrefixFolder: streamFolderPrefix,
            onUpdate: (snapshot) => {
              if (!signal.aborted) queryClient.setQueryData(queryKey, snapshot);
            },
          }),
        enabled,
        retry: false,
        staleTime: 30_000,
        structuralSharing: false,
      };
    }),
  });

  return prefixes.map((prefix, index) => {
    const result = results[index];
    const items = result?.data?.items;
    return {
      prefix,
      items: namespace && items ? sortedOnce(items) : EMPTY,
      isLoading: result?.isLoading ?? true,
      error: (result?.error as Error | null) ?? null,
    };
  });
}

const EMPTY: FileItem[] = [];

/** Listings are immutable snapshots, so each one is sorted once and reused across renders. */
const sortedListings = new WeakMap<FileItem[], FileItem[]>();
export function sortedOnce(items: FileItem[]): FileItem[] {
  let sorted = sortedListings.get(items);
  if (!sorted) {
    sorted = sortColumnItems(items);
    sortedListings.set(items, sorted);
  }
  return sorted;
}
