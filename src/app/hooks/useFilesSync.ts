import { useCallback, useEffect, useMemo, useRef } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { listen } from '@tauri-apps/api/event';
import { startBackgroundSync, cancelBackgroundSync, StorageConfig } from '@/app/lib/r2cache';
import { useFolderSizeStore } from '@/app/stores/folderSizeStore';
import { useSyncStore, SyncPhase } from '@/app/stores/syncStore';

interface BackgroundSyncProgressEvent {
  objects_fetched: number;
  estimated_total: number | null;
  is_running: boolean;
  speed: number;
}

interface BackgroundSyncCompleteEvent {
  total_objects: number;
  cancelled: boolean;
}

/**
 * Files sync hook — now uses background deep sync instead of blocking full sync.
 *
 * The foreground lazy sync (per-folder) is handled by useR2Files directly.
 * This hook manages the background deep sync that fills in the complete dataset
 * for search, folder sizes, and accurate counts.
 *
 * Keeps the same return API (isSyncing, isSynced, lastSyncTime, refresh)
 * so page.tsx doesn't need massive changes.
 */
export function useFilesSync(config: StorageConfig | null) {
  const queryClient = useQueryClient();
  const clearSizes = useFolderSizeStore((state) => state.clearSizes);
  const bgStartedRef = useRef<string | null>(null);

  // Get per-bucket sync time
  const bucketSyncTimes = useSyncStore((state) => state.bucketSyncTimes);
  const lastSyncTime = useMemo(() => {
    if (!config?.accountId || !config?.bucket) return null;
    return useSyncStore.getState().getLastSyncTime(config.accountId, config.bucket);
  }, [config?.accountId, config?.bucket, bucketSyncTimes]);

  // Update current bucket in store when config changes
  useEffect(() => {
    useSyncStore.getState().setCurrentBucket(config?.accountId ?? null, config?.bucket ?? null);
  }, [config?.accountId, config?.bucket]);

  // Listen for background sync progress events
  useEffect(() => {
    const unlistenProgress = listen<BackgroundSyncProgressEvent>(
      'background-sync-progress',
      (event) => {
        useSyncStore.getState().setBackgroundSyncProgress({
          objectsFetched: event.payload.objects_fetched,
          estimatedTotal: event.payload.estimated_total,
          speed: event.payload.speed,
          isRunning: event.payload.is_running,
        });
      }
    );

    const unlistenComplete = listen<BackgroundSyncCompleteEvent>(
      'background-sync-complete',
      (event) => {
        useSyncStore.getState().completeBackgroundSync(event.payload.total_objects);
        useSyncStore.getState().setTotalFiles(event.payload.total_objects);

        // Update sync time
        if (config?.accountId && config?.bucket) {
          useSyncStore.getState().setLastSyncTime(config.accountId, config.bucket, Date.now());
        }

        // Clear folder sizes so they reload from the now-accurate directory tree
        clearSizes();

        // Invalidate folder-contents queries so useR2Files refetches with full data
        if (config) {
          queryClient.invalidateQueries({
            queryKey: ['folder-contents', config.provider, config.accountId, config.bucket],
          });
        }
      }
    );

    const unlistenError = listen<string>('background-sync-error', (event) => {
      useSyncStore.getState().failBackgroundSync(event.payload);
      console.error('Background sync error:', event.payload);
    });

    // Also listen for legacy sync events (used by SyncProgress component)
    const unlistenPhase = listen<SyncPhase>('sync-phase', (event) => {
      useSyncStore.getState().setPhase(event.payload);
    });

    const unlistenSyncProgress = listen<number>('sync-progress', (event) => {
      useSyncStore.getState().setProgress(event.payload);
    });

    const unlistenStore = listen<number>('store-progress', (event) => {
      useSyncStore.getState().setStoredFiles(event.payload);
    });

    return () => {
      unlistenProgress.then((fn) => fn());
      unlistenComplete.then((fn) => fn());
      unlistenError.then((fn) => fn());
      unlistenPhase.then((fn) => fn());
      unlistenSyncProgress.then((fn) => fn());
      unlistenStore.then((fn) => fn());
    };
  }, [config, queryClient]);

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

  // Auto-start background sync when bucket changes
  useEffect(() => {
    if (!isConfigReady || !config) return;

    const bucketKey = `${config.accountId}:${config.bucket}`;
    if (bgStartedRef.current === bucketKey) return; // Already started for this bucket

    bgStartedRef.current = bucketKey;
    useSyncStore.getState().startBackgroundSync();

    startBackgroundSync(config).catch((err) => {
      console.error('Failed to start background sync:', err);
      useSyncStore.getState().failBackgroundSync(err instanceof Error ? err.message : String(err));
    });

    return () => {
      cancelBackgroundSync().catch(() => {});
      bgStartedRef.current = null;
    };
  }, [isConfigReady, config?.accountId, config?.bucket]);

  // Background sync state for return values
  const backgroundSync = useSyncStore((state) => state.backgroundSync);

  const refresh = useCallback(async () => {
    if (!config) return;

    // Cancel current background sync and restart
    try {
      await cancelBackgroundSync();
    } catch {
      // Ignore cancel errors
    }

    clearSizes();
    useSyncStore.getState().resetProgress();
    useSyncStore.getState().resetBackgroundSync();
    bgStartedRef.current = null;

    // Restart background sync
    useSyncStore.getState().startBackgroundSync();
    const bucketKey = `${config.accountId}:${config.bucket}`;
    bgStartedRef.current = bucketKey;

    try {
      await startBackgroundSync(config);
    } catch (err) {
      console.error('Failed to restart background sync:', err);
    }

    // Also invalidate folder-contents so useR2Files refetches current folder
    await queryClient.invalidateQueries({
      queryKey: ['folder-contents', config.provider, config.accountId, config.bucket],
    });
  }, [queryClient, config, clearSizes]);

  return {
    // isSyncing: true while background sync is running
    isSyncing: backgroundSync.isRunning,
    // isSynced: true once we have at least some data (lazy sync sets lastSyncTime)
    isSynced: lastSyncTime !== null,
    syncError: backgroundSync.error ? new Error(backgroundSync.error) : null,
    lastSyncTime,
    refresh,
  };
}
