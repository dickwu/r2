import { t as i18nT } from '@/i18n';
import { useCallback, useEffect, useMemo, useRef } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import {
  getPrefixCache,
  hasSigningCredentials,
  streamFolderPrefix,
  type StorageConfig,
} from '@/app/lib/r2cache';
import { useStorageNamespace } from '@/app/hooks/useStorageNamespace';
import { useSyncStore } from '@/app/stores/syncStore';
import { useCurrentPathStore } from '@/app/stores/currentPathStore';
import { useMoveStore, type MoveStatusChangedEvent } from '@/app/stores/moveStore';
import { loadFolderItems, type FolderSnapshot } from '@/app/utils/folderItems';
import { affectedMoveFolders, createFolderInvalidator, parentPrefix } from '@/app/utils/syncScope';

export type { FileItem } from '@/app/utils/folderItems';

export function useR2Files(config: StorageConfig | null, prefix: string = '') {
  const queryClient = useQueryClient();
  const forceNextRefresh = useRef(false);
  const namespace = useStorageNamespace(config);
  const provider = config?.provider;
  const accountId = config?.accountId;
  const bucket = config?.bucket;
  const queryKey = useMemo(
    () => ['folder-contents', provider, accountId, bucket, prefix, namespace],
    [provider, accountId, bucket, prefix, namespace]
  );

  // Abort the in-flight listing when the observed folder changes (bucket or
  // prefix). Without this, a finished request for bucket A can still settle
  // after the user has switched to B; the query key keeps that result off the
  // screen, but the leftover work and events should not keep running.
  useEffect(() => {
    return () => {
      void queryClient.cancelQueries({ queryKey });
    };
  }, [queryClient, queryKey]);

  useEffect(() => {
    if (!namespace || provider === undefined || accountId === undefined) return;
    const effectProvider = provider;
    const effectAccount = accountId;
    return () => {
      const predicate = (query: { queryKey: readonly unknown[] }) =>
        query.queryKey[0] === 'folder-contents' &&
        query.queryKey[1] === effectProvider &&
        query.queryKey[2] === effectAccount &&
        query.queryKey[5] === namespace;
      void queryClient.cancelQueries({ predicate });
      queryClient.removeQueries({ predicate });
    };
  }, [namespace, provider, accountId, queryClient]);

  const query = useQuery({
    queryKey,
    queryFn: async ({ signal }): Promise<FolderSnapshot> => {
      if (!config) throw new Error(i18nT('errors.notConfigured'));
      const forceRefresh = forceNextRefresh.current;
      forceNextRefresh.current = false;
      return loadFolderItems({
        config,
        prefix,
        signal,
        forceRefresh,
        fallbackSnapshot: queryClient.getQueryData<FolderSnapshot>(queryKey),
        readCachedFolder: getPrefixCache,
        readPrefixFolder: streamFolderPrefix,
        onUpdate: (snapshot) => {
          if (!signal.aborted) queryClient.setQueryData(queryKey, snapshot);
        },
      });
    },
    enabled: hasSigningCredentials(config) && namespace !== null,
    // The backend owns the bounded network retry budget. Do not multiply it here.
    retry: false,
    staleTime: 30_000,
    // The page accumulator already preserves immutable snapshots and reuses
    // unchanged items. Deep reconciliation of each growing/reordered page
    // otherwise copies the same directory repeatedly and blocks first paint.
    structuralSharing: false,
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
    if (provider === undefined || accountId === undefined || bucket === undefined) return;
    const prefixes = new Set([
      ...cacheUpdatedPaths,
      ...removedPaths.map(parentPrefix),
      ...createdPaths.map(parentPrefix),
    ]);
    invalidator.add(
      Array.from(prefixes, (path) => ['folder-contents', provider, accountId, bucket, path])
    );
  }, [provider, accountId, bucket, cacheUpdatedPaths, removedPaths, createdPaths, invalidator]);

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
    forceNextRefresh.current = true;
    await queryClient.invalidateQueries({ queryKey });
  }, [queryClient, queryKey]);

  return {
    items: namespace ? (query.data?.items ?? []) : [],
    hasData: namespace !== null && query.data !== undefined,
    isLoading: query.isLoading,
    isFetching: query.isFetching,
    isPartial: query.data?.complete === false,
    isCached: query.data?.fromCache ?? false,
    freshness: query.data?.freshness,
    error: query.error,
    refresh,
  };
}
