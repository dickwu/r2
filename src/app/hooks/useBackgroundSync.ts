import { useEffect, useCallback } from 'react';
import { listen } from '@tauri-apps/api/event';
import { useQueryClient } from '@tanstack/react-query';
import { startBackgroundSync, cancelBackgroundSync, StorageConfig } from '@/app/lib/r2cache';
import { useSyncStore } from '@/app/stores/syncStore';

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
 * Manages background deep sync lifecycle.
 * Starts automatically when a bucket is selected.
 * Listens to progress events and updates syncStore.
 */
export function useBackgroundSync(config: StorageConfig | null) {
  const queryClient = useQueryClient();
  const store = useSyncStore;

  // Listen for background sync events
  useEffect(() => {
    const unlistenProgress = listen<BackgroundSyncProgressEvent>(
      'background-sync-progress',
      (event) => {
        store.getState().setBackgroundSyncProgress({
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
        store.getState().completeBackgroundSync(event.payload.total_objects);

        // Invalidate all folder-contents queries so they refetch with full data
        if (config) {
          queryClient.invalidateQueries({
            queryKey: ['folder-contents', config.provider, config.accountId, config.bucket],
          });
        }
      }
    );

    const unlistenError = listen<string>('background-sync-error', (event) => {
      store.getState().failBackgroundSync(event.payload);
    });

    return () => {
      unlistenProgress.then((fn) => fn());
      unlistenComplete.then((fn) => fn());
      unlistenError.then((fn) => fn());
    };
  }, [config, queryClient, store]);

  // Start background sync when config changes (new bucket selected)
  const start = useCallback(async () => {
    if (!config) return;

    const bgState = store.getState().backgroundSync;
    if (bgState.isRunning) return; // Already running

    store.getState().startBackgroundSync();

    try {
      await startBackgroundSync(config);
    } catch (error) {
      store.getState().failBackgroundSync(error instanceof Error ? error.message : String(error));
    }
  }, [config, store]);

  const cancel = useCallback(async () => {
    try {
      await cancelBackgroundSync();
    } catch (error) {
      console.error('Failed to cancel background sync:', error);
    }
  }, []);

  return {
    start,
    cancel,
    backgroundSync: store((state) => state.backgroundSync),
  };
}
