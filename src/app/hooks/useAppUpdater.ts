import { useEffect } from 'react';
import { useShallow } from 'zustand/react/shallow';
import { useUpdateStore } from '@/app/stores/updateStore';

const FIRST_CHECK_DELAY_MS = 3_000;
const CHECK_INTERVAL_MS = 30 * 60 * 1_000;
/** How long "You're up to date" and a failed check stay in the status bar. */
const UP_TO_DATE_MS = 4_000;
const CHECK_FAILED_MS = 10_000;

/**
 * Runs the updater for the app's lifetime: reads the version (and whether this
 * launch follows an update), checks 3 seconds after launch and every 30
 * minutes, and lets a check's result fade from the status bar. Mount it once,
 * in the status bar; everything else reads `useUpdateStore`.
 */
export function useAppUpdater() {
  const state = useUpdateStore(
    useShallow((s) => ({
      phase: s.phase,
      manualCheck: s.manualCheck,
      currentVersion: s.currentVersion,
      update: s.update,
      progress: s.progress,
      failure: s.failure,
      checkForUpdates: s.checkForUpdates,
      openDialog: s.openDialog,
      retry: s.retry,
    }))
  );

  useEffect(() => {
    const store = useUpdateStore.getState();
    void store.init();
    if (process.env.NODE_ENV !== 'production') {
      void import('@/app/lib/updates/devUpdaterSeam').then((m) => m.installDevUpdaterSeam());
    }
    const first = setTimeout(() => void store.checkForUpdates(), FIRST_CHECK_DELAY_MS);
    const interval = setInterval(() => void store.checkForUpdates(), CHECK_INTERVAL_MS);
    return () => {
      clearTimeout(first);
      clearInterval(interval);
    };
  }, []);

  const { phase, failure } = state;
  useEffect(() => {
    const transient =
      phase === 'upToDate'
        ? UP_TO_DATE_MS
        : phase === 'error' && failure?.step === 'check'
          ? CHECK_FAILED_MS
          : 0;
    if (!transient) return;
    const timer = setTimeout(() => useUpdateStore.getState().clearCheckResult(), transient);
    return () => clearTimeout(timer);
  }, [phase, failure]);

  return state;
}
