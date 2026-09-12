import { useCallback, useEffect, useMemo } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import {
  getPrefixCache,
  hasSigningCredentials,
  streamFolderPrefix,
  type StorageConfig,
} from '@/app/lib/r2cache';
import { useSyncStore } from '@/app/stores/syncStore';
import { useCurrentPathStore } from '@/app/stores/currentPathStore';
import { useMoveStore, type MoveStatusChangedEvent } from '@/app/stores/moveStore';
import { loadFolderItems, type FolderSnapshot } from '@/app/utils/folderItems';
import { affectedMoveFolders, createFolderInvalidator, parentPrefix } from '@/app/utils/syncScope';

export type { FileItem } from '@/app/utils/folderItems';

export function useR2Files(config: StorageConfig | null, prefix: string = '') {
  const queryClient = useQueryClient();
  const queryKey = useMemo(
    () => ['folder-contents', config?.provider, config?.accountId, config?.bucket, prefix],
    [config?.provider, config?.accountId, config?.bucket, prefix]
  );

  const query = useQuery({
    queryKey,
    queryFn: async ({ signal }): Promise<FolderSnapshot> => {
      if (!config) throw new Error('Storage account is not configured');
      return loadFolderItems({
        config,
        prefix,
        signal,
        readCachedFolder: getPrefixCache,
        readPrefixFolder: streamFolderPrefix,
        onUpdate: (snapshot) => {
          if (!signal.aborted) queryClient.setQueryData(queryKey, snapshot);
        },
      });
    },
    enabled: hasSigningCredentials(config),
    // The backend owns the bounded network retry budget. Do not multiply it here.
    retry: false,
    staleTime: 30_000,
    // Previous-directory rows are never actionable placeholders for this scope.
  });

  useEffect(() => {
    useSyncStore.getState().setIsFolderLoading(query.isFetching);
  }, [query.isFetching]);

  const cacheUpdatedPaths = useCurrentPathStore((state) => state.cacheUpdatedPaths);
  const removedPaths = useCurrentPathStore((state) => state.removedPaths);
  const createdPaths = useCurrentPathStore((state) => state.createdPaths);
  const invalidator = useMemo(
    () =>
      createFolderInvalidator((key) => {
        void queryClient.invalidateQueries({ queryKey: key });
      }),
    [queryClient]
  );

  useEffect(() => () => invalidator.dispose(), [invalidator]);

  useEffect(() => {
    if (!config) return;
    const prefixes = new Set([
      ...cacheUpdatedPaths,
      ...removedPaths.map(parentPrefix),
      ...createdPaths.map(parentPrefix),
    ]);
    invalidator.add(
      Array.from(prefixes, (path) => [
        'folder-contents',
        config.provider,
        config.accountId,
        config.bucket,
        path,
      ])
    );
  }, [
    config?.provider,
    config?.accountId,
    config?.bucket,
    cacheUpdatedPaths,
    removedPaths,
    createdPaths,
    invalidator,
  ]);

  useEffect(() => {
    let disposed = false;
    let unlisten: UnlistenFn | undefined;
    void listen<MoveStatusChangedEvent>('move-status-changed', ({ payload }) => {
      if (disposed || payload.status !== 'success') return;
      if (
        payload.source_provider &&
        payload.source_account_id &&
        payload.source_bucket &&
        payload.source_key &&
        payload.dest_provider &&
        payload.dest_account_id &&
        payload.dest_bucket &&
        payload.dest_key
      ) {
        invalidator.add(
          affectedMoveFolders({
            sourceProvider: payload.source_provider,
            sourceAccountId: payload.source_account_id,
            sourceBucket: payload.source_bucket,
            sourceKey: payload.source_key,
            destProvider: payload.dest_provider,
            destAccountId: payload.dest_account_id,
            destBucket: payload.dest_bucket,
            destKey: payload.dest_key,
            deleteOriginal: payload.delete_original ?? true,
          })
        );
      } else {
        const task = useMoveStore
          .getState()
          .tasks.find((candidate) => candidate.id === payload.task_id);
        if (task) invalidator.add(affectedMoveFolders(task));
      }
    })
      .then((remove) => {
        if (disposed) remove();
        else unlisten = remove;
      })
      .catch((error) => console.warn('Unable to listen for moved files', error));
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [invalidator]);

  const refresh = useCallback(async () => {
    await queryClient.invalidateQueries({ queryKey });
  }, [queryClient, queryKey]);

  return {
    items: query.data?.items ?? [],
    hasData: query.data !== undefined,
    isLoading: query.isLoading,
    isFetching: query.isFetching,
    isPartial: query.data?.complete === false,
    isCached: query.data?.fromCache ?? false,
    freshness: query.data?.freshness,
    error: query.error,
    refresh,
  };
}
