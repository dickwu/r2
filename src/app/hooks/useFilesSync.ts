import { useCallback, useEffect, useMemo, useRef } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import {
  startBackgroundSync,
  cancelBackgroundSync,
  hasSigningCredentials,
  type StorageConfig,
} from '@/app/lib/r2cache';
import { useFolderSizeStore } from '@/app/stores/folderSizeStore';
import { useSyncStore } from '@/app/stores/syncStore';
import { logSession } from '@/app/lib/diagnostics/sessionLog';
import { matchesSyncRun, type SyncScope } from '@/app/utils/syncScope';

interface BackgroundSyncProgressEvent extends SyncScope {
  objects_fetched: number;
  bytes_fetched: number;
  estimated_total: number | null;
  is_running: boolean;
  speed: number;
}

interface BackgroundSyncCompleteEvent extends SyncScope {
  total_objects: number;
  total_bytes: number;
  cancelled: boolean;
  skipped_prefixes?: string[];
}

interface SyncSession {
  config: StorageConfig;
  disposed: boolean;
  ready: Promise<void>;
  run: SyncScope | null;
}

const describeError = (error: unknown) => (error instanceof Error ? error.message : String(error));

export function useFilesSync(config: StorageConfig | null) {
  const queryClient = useQueryClient();
  const sessionRef = useRef<SyncSession | null>(null);
  const bucketSyncTimes = useSyncStore((state) => state.bucketSyncTimes);
  // Providers may use the same account label; keep their sync histories separate.
  const storeAccountId = config ? `${config.provider}:${config.accountId}` : null;
  const lastSyncTime = useMemo(
    () =>
      storeAccountId && config?.bucket
        ? useSyncStore.getState().getLastSyncTime(storeAccountId, config.bucket)
        : null,
    [storeAccountId, config?.bucket, bucketSyncTimes]
  );

  const startRun = useCallback(async (session: SyncSession) => {
    const previousRun = session.run;
    const run: SyncScope = {
      provider: session.config.provider,
      account_id: session.config.accountId,
      bucket: session.config.bucket,
      prefix: '',
      run_id: crypto.randomUUID(),
    };
    session.run = run;
    const current = () => !session.disposed && matchesSyncRun(session.run, run);
    try {
      if (previousRun) await cancelBackgroundSync(previousRun.run_id);
      await session.ready;
      if (!current()) return;
      useSyncStore.getState().resetProgress();
      useSyncStore.getState().startBackgroundSync();
      await startBackgroundSync(session.config, run.run_id);
    } catch (error) {
      if (!current()) return;
      const message = describeError(error);
      useSyncStore.getState().failBackgroundSync(message);
      logSession('app', 'error', `Background sync error: ${message}`);
    }
  }, []);

  useEffect(() => {
    useSyncStore.getState().setCurrentBucket(storeAccountId, config?.bucket ?? null);
    useSyncStore.getState().resetBackgroundSync();
    if (!config || !hasSigningCredentials(config)) return;
    const session: SyncSession = { config, disposed: false, ready: Promise.resolve(), run: null };
    sessionRef.current = session;
    const unlisteners: UnlistenFn[] = [];
    const accepts = (payload: SyncScope) =>
      !session.disposed && matchesSyncRun(session.run, payload);
    const addListener = async <Payload>(name: string, receive: (payload: Payload) => void) => {
      const unlisten = await listen<Payload>(name, (event) => receive(event.payload));
      if (session.disposed) unlisten();
      else unlisteners.push(unlisten);
    };
    session.ready = Promise.all([
      addListener<BackgroundSyncProgressEvent>('background-sync-progress', (event) => {
        if (!accepts(event)) return;
        useSyncStore.getState().setBackgroundSyncProgress({
          objectsFetched: event.objects_fetched,
          bytesFetched: event.bytes_fetched,
          estimatedTotal: event.estimated_total,
          speed: event.speed,
          isRunning: event.is_running,
        });
      }),
      addListener<BackgroundSyncCompleteEvent>('background-sync-complete', (event) => {
        if (!accepts(event)) return;
        session.run = null;
        if (event.cancelled) {
          useSyncStore.getState().resetBackgroundSync();
          return;
        }
        useSyncStore.getState().completeBackgroundSync(event.total_objects, event.total_bytes);
        useSyncStore.getState().setTotalFiles(event.total_objects);
        const skipped = event.skipped_prefixes ?? [];
        if (skipped.length > 0) {
          logSession(
            'app',
            'warn',
            `Sync finished with ${skipped.length} skipped folder(s): ${skipped.join(', ')}`
          );
        }
        if (skipped.length === 0) {
          useSyncStore
            .getState()
            .setLastSyncTime(`${event.provider}:${event.account_id}`, event.bucket, Date.now());
        }
        useFolderSizeStore.getState().clearSizes();
        // The foreground query already revalidates independently. Keep it fresh
        // without interrupting an in-progress page stream when indexing ends.
        void queryClient.invalidateQueries({
          queryKey: ['folder-contents', event.provider, event.account_id, event.bucket],
          refetchType: 'none',
        });
      }),
      addListener<SyncScope & { error: string }>('background-sync-error', (event) => {
        if (!accepts(event)) return;
        session.run = null;
        useSyncStore.getState().failBackgroundSync(event.error);
        logSession('app', 'error', `Background sync error: ${event.error}`);
      }),
      addListener<SyncScope>('background-sync-cancelled', (event) => {
        if (!accepts(event)) return;
        session.run = null;
        useSyncStore.getState().resetBackgroundSync();
      }),
    ]).then(() => {});
    void startRun(session);

    return () => {
      session.disposed = true;
      if (sessionRef.current === session) sessionRef.current = null;
      unlisteners.forEach((unlisten) => unlisten());
      if (session.run) void cancelBackgroundSync(session.run.run_id).catch(() => {});
    };
  }, [config, queryClient, startRun, storeAccountId]);

  const refresh = useCallback(async () => {
    const session = sessionRef.current;
    if (!session || session.disposed) return;
    useFolderSizeStore.getState().clearSizes();
    await startRun(session);
    if (!session.disposed)
      await queryClient.invalidateQueries({
        queryKey: [
          'folder-contents',
          session.config.provider,
          session.config.accountId,
          session.config.bucket,
        ],
      });
  }, [queryClient, startRun]);

  const backgroundSync = useSyncStore((state) => state.backgroundSync);
  return {
    isSyncing: backgroundSync.isRunning,
    isSynced: lastSyncTime !== null || backgroundSync.objectsFetched > 0,
    syncError: backgroundSync.error ? new Error(backgroundSync.error) : null,
    lastSyncTime,
    refresh,
  };
}
