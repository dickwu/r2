import { useUpdateStore } from '@/app/stores/updateStore';
import { useEditorDirtyStore } from '@/app/stores/editorDirtyStore';
import { RELEASES_URL } from './releaseNotes';
import { clearUpdatePrefs, setLastRunVersion } from './updatePrefs';
import {
  resetUpdaterPlugins,
  setUpdaterPlugins,
  updaterPlugins,
  type AvailableUpdate,
  type DownloadEvent,
} from './updaterPlugins';

/**
 * Dev builds only: `window.__r2UpdaterTest` fakes an update end to end, so the
 * dialog, progress, failures, and What's new can be driven from the devtools
 * console or tauri-connector without publishing a release. Every method returns
 * at once; read `state()` to follow along.
 *
 *   __r2UpdaterTest.simulate({ version: '0.3.18', durationMs: 4000 })
 *   __r2UpdaterTest.simulate({ failAt: 'download', failWith: 'error sending request' })
 *   __r2UpdaterTest.unsavedEdits(true)
 *   __r2UpdaterTest.whatsNew('0.3.16')
 *   __r2UpdaterTest.useReleases('http://127.0.0.1:8787/releases.json')
 */

export interface SimulateOptions {
  version?: string;
  body?: string;
  /** Pretend the running app is this version. */
  currentVersion?: string;
  sizeBytes?: number;
  durationMs?: number;
  failAt?: 'check' | 'download' | 'install' | 'restart';
  failWith?: string;
  /** No Content-Length: progress shows bytes only. */
  unknownSize?: boolean;
  /** Run as a check you asked for (default) or a background one. */
  manual?: boolean;
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

const fakeUpdate = (
  options: Required<Pick<SimulateOptions, 'version' | 'sizeBytes' | 'durationMs'>> & SimulateOptions
): AvailableUpdate => ({
  version: options.version,
  body: options.body,
  date: new Date().toISOString(),
  downloadAndInstall: async (onEvent?: (event: DownloadEvent) => void) => {
    const steps = 40;
    const chunk = Math.ceil(options.sizeBytes / steps);
    onEvent?.({
      event: 'Started',
      data: { contentLength: options.unknownSize ? undefined : options.sizeBytes },
    });
    for (let i = 0; i < steps; i++) {
      await sleep(options.durationMs / steps);
      if (options.failAt === 'download' && i === Math.floor(steps * 0.6)) {
        throw new Error(
          options.failWith ??
            'error sending request for url (https://github.com/…): connection reset by peer'
        );
      }
      onEvent?.({ event: 'Progress', data: { chunkLength: chunk } });
    }
    onEvent?.({ event: 'Finished' });
    await sleep(800);
    if (options.failAt === 'install') {
      throw new Error(options.failWith ?? 'Permission denied (os error 13)');
    }
  },
});

let relaunches = 0;

const seam = {
  simulate(options: SimulateOptions = {}) {
    const version = options.version ?? '0.3.18';
    const update = fakeUpdate({ sizeBytes: 9_800_000, durationMs: 4_000, ...options, version });
    setUpdaterPlugins({
      check: async () => {
        await sleep(600);
        if (options.failAt === 'check')
          throw new Error(options.failWith ?? 'error sending request for url: dns error');
        return update;
      },
      relaunch: async () => {
        await sleep(300);
        if (options.failAt === 'restart') throw new Error(options.failWith ?? 'failed to restart');
        relaunches += 1;
        // A real relaunch reloads the app; here the state just says so.
        useUpdateStore.setState({ phase: 'idle', update: null, dialogOpen: false });
      },
    });
    if (options.currentVersion) useUpdateStore.setState({ currentVersion: options.currentVersion });
    void useUpdateStore.getState().checkForUpdates({ manual: options.manual ?? true });
    return 'checking';
  },
  upToDate() {
    setUpdaterPlugins({ check: async () => (await sleep(600), null) });
    void useUpdateStore.getState().checkForUpdates({ manual: true });
    return 'checking';
  },
  /** Show What's new as if this launch followed an update from `from`. */
  whatsNew(from: string, to?: string) {
    const current = to ?? useUpdateStore.getState().currentVersion ?? '0.3.18';
    setLastRunVersion(current);
    useUpdateStore.setState({ whatsNew: { from, to: current }, currentVersion: current });
    void useUpdateStore.getState().loadFeed();
    return 'shown';
  },
  unsavedEdits(on: boolean) {
    useEditorDirtyStore.getState().setDirty('dev-seam', on);
    return on;
  },
  useReleases(url: string = RELEASES_URL) {
    setUpdaterPlugins({ releasesUrl: url });
    useUpdateStore.setState({ feed: null, feedStatus: 'idle' });
    void useUpdateStore.getState().loadFeed({ force: true });
    return updaterPlugins().releasesUrl;
  },
  state() {
    const s = useUpdateStore.getState();
    return JSON.stringify({
      phase: s.phase,
      currentVersion: s.currentVersion,
      update: s.update?.version ?? null,
      progress: s.progress,
      failure: s.failure && {
        step: s.failure.step,
        kind: s.failure.kind,
        message: s.failure.message,
      },
      dialogOpen: s.dialogOpen,
      releaseNotesOpen: s.releaseNotesOpen,
      feedStatus: s.feedStatus,
      feedVersions: s.feed?.releases.length ?? 0,
      whatsNew: s.whatsNew,
      relaunches,
    });
  },
  reset() {
    resetUpdaterPlugins();
    clearUpdatePrefs();
    useEditorDirtyStore.getState().setDirty('dev-seam', false);
    useUpdateStore.setState({
      phase: 'idle',
      update: null,
      failure: null,
      dialogOpen: false,
      releaseNotesOpen: false,
      whatsNew: null,
      manualCheck: false,
    });
    return 'reset';
  },
};

declare global {
  interface Window {
    __r2UpdaterTest?: typeof seam;
  }
}

export const installDevUpdaterSeam = (): void => {
  if (typeof window !== 'undefined') window.__r2UpdaterTest = seam;
};
