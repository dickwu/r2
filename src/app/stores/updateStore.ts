import { create } from 'zustand';
import { logSession } from '@/app/lib/diagnostics/sessionLog';
import {
  entriesToMarkdown,
  fetchReleaseFeed,
  releasesBetween,
  type ReleaseFeed,
} from '@/app/lib/updates/releaseNotes';
import {
  markPrompted,
  shouldAutoPrompt,
  skipVersion,
  takeUpgradedFrom,
} from '@/app/lib/updates/updatePrefs';
import { describeUpdateError, type UpdateFailure } from '@/app/lib/updates/updateErrors';
import { updaterPlugins, type AvailableUpdate } from '@/app/lib/updates/updaterPlugins';

/**
 * The app update, start to finish:
 *
 *   idle ─check─▶ checking ─▶ available ─install─▶ downloading ─▶ installing
 *                    │                                   │             │
 *                    ├─▶ upToDate (a check you asked for) │             ├─▶ restarting (Update and restart)
 *                    └─▶ error (a check you asked for)    └──── error ◀─┴─▶ readyToRestart (Install now, restart later)
 *
 * Background checks never surface an error; they leave the last good state.
 * Actions ignore repeats while a step is running, so a double click cannot
 * start a second download.
 */

export type UpdatePhase =
  | 'idle'
  | 'checking'
  | 'upToDate'
  | 'available'
  | 'downloading'
  | 'installing'
  | 'readyToRestart'
  | 'restarting'
  | 'error';

export interface UpdateInfo {
  version: string;
  date?: string;
  body?: string;
}

export interface DownloadProgress {
  downloaded: number;
  /** Null when the server sent no Content-Length. */
  total: number | null;
}

export type FeedStatus = 'idle' | 'loading' | 'ready' | 'failed';

interface UpdateState {
  currentVersion: string | null;
  phase: UpdatePhase;
  /** The check running or just finished was one you asked for, so its result shows. */
  manualCheck: boolean;
  update: UpdateInfo | null;
  progress: DownloadProgress;
  failure: UpdateFailure | null;
  /** What the running install does when it finishes. */
  restartAfterInstall: boolean;
  dialogOpen: boolean;
  releaseNotesOpen: boolean;
  feed: ReleaseFeed | null;
  feedStatus: FeedStatus;
  /** Set on the first launch after an upgrade. */
  whatsNew: { from: string; to: string } | null;
  lastCheckedAt: number | null;

  init(): Promise<void>;
  checkForUpdates(options?: { manual?: boolean }): Promise<void>;
  install(options: { restart: boolean }): Promise<void>;
  restartNow(): Promise<void>;
  retry(): Promise<void>;
  skipThisVersion(): void;
  openDialog(): void;
  closeDialog(): void;
  clearCheckResult(): void;
  loadFeed(options?: { force?: boolean }): Promise<ReleaseFeed | null>;
  openReleaseNotes(): void;
  closeReleaseNotes(): void;
  dismissWhatsNew(): void;
}

/** The plugin's Update resource: not state, since nothing renders from it. */
let pending: AvailableUpdate | null = null;
let initStarted = false;
let feedRequest: Promise<ReleaseFeed | null> | null = null;

const BUSY: ReadonlySet<UpdatePhase> = new Set([
  'checking',
  'downloading',
  'installing',
  'restarting',
]);

export const isUpdateBusy = (phase: UpdatePhase): boolean => BUSY.has(phase);

const initialState = {
  currentVersion: null,
  phase: 'idle' as UpdatePhase,
  manualCheck: false,
  update: null,
  progress: { downloaded: 0, total: null },
  failure: null,
  restartAfterInstall: true,
  dialogOpen: false,
  releaseNotesOpen: false,
  feed: null,
  feedStatus: 'idle' as FeedStatus,
  whatsNew: null,
  lastCheckedAt: null,
};

export const useUpdateStore = create<UpdateState>((set, get) => ({
  ...initialState,

  init: async () => {
    if (initStarted) return;
    initStarted = true;
    let version: string | null = null;
    try {
      version = await updaterPlugins().getVersion();
    } catch {
      // Not running inside Tauri (plain `next dev` in a browser).
    }
    if (!version) return;
    const from = takeUpgradedFrom(version);
    set({ currentVersion: version, whatsNew: from ? { from, to: version } : null });
    if (from) {
      logSession('app', 'info', `Updated from v${from} to v${version}`);
      void get().loadFeed();
    }
  },

  checkForUpdates: async ({ manual = false } = {}) => {
    const { phase, dialogOpen, failure } = get();
    if (BUSY.has(phase)) {
      if (manual && phase !== 'checking') set({ dialogOpen: true });
      return;
    }
    if (phase === 'readyToRestart') {
      if (manual) set({ dialogOpen: true });
      return;
    }
    // A background check must not replace a failed download the dialog is showing.
    if (!manual && (dialogOpen || (phase === 'error' && failure?.step !== 'check'))) return;

    set({ phase: 'checking', manualCheck: manual, failure: null });
    try {
      const found = await updaterPlugins().check();
      if (!found) {
        pending = null;
        set({
          phase: manual ? 'upToDate' : 'idle',
          update: null,
          lastCheckedAt: Date.now(),
        });
        return;
      }
      pending = found;
      const version = found.version;
      const prompt = manual || shouldAutoPrompt(version);
      if (prompt && !manual) markPrompted(version);
      set((state) => ({
        phase: 'available',
        update: { version, date: found.date, body: found.body },
        lastCheckedAt: Date.now(),
        dialogOpen: state.dialogOpen || prompt,
      }));
      logSession('app', 'info', `Update v${version} available`);
      void get().loadFeed();
    } catch (error) {
      const described = describeUpdateError('check', error);
      logSession('app', 'warn', `Update check failed: ${described.detail}`);
      if (manual) {
        set({ phase: 'error', failure: described, lastCheckedAt: Date.now() });
      } else {
        set({ phase: pending ? 'available' : 'idle', lastCheckedAt: Date.now() });
      }
    }
  },

  install: async ({ restart }) => {
    const update = pending;
    const { phase } = get();
    if (!update || BUSY.has(phase) || phase === 'readyToRestart') return;

    set({
      phase: 'downloading',
      progress: { downloaded: 0, total: null },
      failure: null,
      restartAfterInstall: restart,
      manualCheck: false,
    });
    logSession('app', 'info', `Downloading update v${update.version}`);

    let step: 'download' | 'install' = 'download';
    let downloaded = 0;
    let total: number | null = null;
    let shownPercent = -1;
    let shownAt = 0;
    try {
      await update.downloadAndInstall((event) => {
        if (event.event === 'Started') {
          total =
            event.data.contentLength && event.data.contentLength > 0
              ? event.data.contentLength
              : null;
          set({ progress: { downloaded: 0, total } });
        } else if (event.event === 'Progress') {
          downloaded += event.data.chunkLength;
          // Chunks arrive every few KB; repaint per whole percent, or 4x a second without a size.
          const percent = total ? Math.floor((downloaded / total) * 100) : -1;
          const now = Date.now();
          if (total ? percent !== shownPercent : now - shownAt >= 250) {
            shownPercent = percent;
            shownAt = now;
            set({ progress: { downloaded, total } });
          }
        } else if (event.event === 'Finished') {
          step = 'install';
          set({ phase: 'installing', progress: { downloaded, total } });
        }
      });
    } catch (error) {
      const described = describeUpdateError(step, error);
      logSession('app', 'error', `Update ${step} failed: ${described.detail}`);
      set({ phase: 'error', failure: described });
      return;
    }

    logSession('app', 'info', `Update v${update.version} installed`);
    set({ phase: 'installing' });
    if (get().restartAfterInstall) {
      await get().restartNow();
    } else {
      set({ phase: 'readyToRestart' });
    }
  },

  restartNow: async () => {
    const { phase, failure } = get();
    const installed =
      phase === 'readyToRestart' ||
      phase === 'installing' ||
      (phase === 'error' && failure?.step === 'restart');
    if (!installed) return;
    set({ phase: 'restarting', failure: null });
    try {
      await updaterPlugins().relaunch();
    } catch (error) {
      const described = describeUpdateError('restart', error);
      logSession('app', 'error', `Restart after update failed: ${described.detail}`);
      set({ phase: 'error', failure: described, dialogOpen: true });
    }
  },

  retry: async () => {
    const { failure, restartAfterInstall } = get();
    if (!failure || get().phase !== 'error') return;
    if (failure.step === 'check') return get().checkForUpdates({ manual: true });
    if (failure.step === 'restart') return get().restartNow();
    set({ phase: 'available', failure: null });
    return get().install({ restart: restartAfterInstall });
  },

  skipThisVersion: () => {
    const version = get().update?.version;
    if (version) {
      skipVersion(version);
      logSession('app', 'info', `Skipped update v${version}`);
    }
    set({ dialogOpen: false });
  },

  openDialog: () => {
    if (get().update) set({ dialogOpen: true });
  },

  closeDialog: () => {
    const { phase, failure } = get();
    // Closing a failed check or download returns to the offer; the status bar keeps it.
    if (phase === 'error' && failure?.step !== 'restart') {
      set({ dialogOpen: false, phase: pending ? 'available' : 'idle', failure: null });
    } else {
      set({ dialogOpen: false });
    }
  },

  clearCheckResult: () => {
    const { phase, failure } = get();
    if (phase === 'upToDate' || (phase === 'error' && failure?.step === 'check')) {
      set({ phase: pending ? 'available' : 'idle', manualCheck: false, failure: null });
    }
  },

  loadFeed: async ({ force = false } = {}) => {
    const { feed, feedStatus } = get();
    if (feedRequest) return feedRequest;
    if (feedStatus === 'ready' && !force) return feed;
    set({ feedStatus: 'loading' });
    const plugins = updaterPlugins();
    feedRequest = fetchReleaseFeed(plugins.fetch, plugins.releasesUrl).then((loaded) => {
      feedRequest = null;
      if (loaded) set({ feed: loaded, feedStatus: 'ready' });
      else set({ feedStatus: feed ? 'ready' : 'failed' });
      return loaded ?? feed;
    });
    return feedRequest;
  },

  openReleaseNotes: () => {
    set({ releaseNotesOpen: true });
    void get().loadFeed();
  },
  closeReleaseNotes: () => set({ releaseNotesOpen: false }),
  dismissWhatsNew: () => set({ whatsNew: null }),
}));

export type NotesSource = 'feed' | 'body' | 'none';

/**
 * What the update dialog shows: the feed's entries for every version the update
 * brings, else the notes in `latest.json`, else nothing (the dialog links to GitHub).
 */
export const updateNotes = (
  feed: ReleaseFeed | null,
  currentVersion: string | null,
  update: UpdateInfo | null
): { source: NotesSource; markdown: string } => {
  if (!update) return { source: 'none', markdown: '' };
  if (feed) {
    const entries = releasesBetween(feed, currentVersion ?? '0.0.0', update.version);
    if (entries.length > 0) {
      return {
        source: 'feed',
        markdown: entriesToMarkdown(entries, { versionHeadings: entries.length > 1 }),
      };
    }
  }
  const body = update.body?.trim();
  return body ? { source: 'body', markdown: body } : { source: 'none', markdown: '' };
};

/** Tests only: back to a fresh launch. */
export const resetUpdateStoreForTests = (): void => {
  pending = null;
  initStarted = false;
  feedRequest = null;
  useUpdateStore.setState(initialState);
};
