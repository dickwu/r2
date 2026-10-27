import type { FetchLike } from './releaseNotes';
import { RELEASES_URL } from './releaseNotes';

/**
 * Everything the updater needs from Tauri, behind one seam. The real plugins
 * load lazily (they touch `window.__TAURI__` on import); tests and the dev
 * simulator (`window.__r2UpdaterTest`, see devUpdaterSeam.ts) replace parts
 * with `setUpdaterPlugins`.
 */

export type DownloadEvent =
  | { event: 'Started'; data: { contentLength?: number } }
  | { event: 'Progress'; data: { chunkLength: number } }
  | { event: 'Finished' };

/** The parts of plugin-updater's `Update` the app uses. */
export interface AvailableUpdate {
  version: string;
  currentVersion?: string;
  date?: string;
  body?: string;
  downloadAndInstall(onEvent?: (event: DownloadEvent) => void): Promise<void>;
}

export interface UpdaterPlugins {
  getVersion(): Promise<string>;
  check(): Promise<AvailableUpdate | null>;
  relaunch(): Promise<void>;
  fetch: FetchLike;
  openUrl(url: string): Promise<void>;
  releasesUrl: string;
}

const tauriPlugins: UpdaterPlugins = {
  getVersion: async () => (await import('@tauri-apps/api/app')).getVersion(),
  check: async () => {
    const { check } = await import('@tauri-apps/plugin-updater');
    const update = await check();
    if (!update) return null;
    return {
      version: update.version,
      currentVersion: update.currentVersion,
      date: update.date,
      body: update.body,
      downloadAndInstall: (onEvent) =>
        update.downloadAndInstall(onEvent as Parameters<typeof update.downloadAndInstall>[0]),
    };
  },
  relaunch: async () => (await import('@tauri-apps/plugin-process')).relaunch(),
  // plugin-http, not window.fetch: the webview origin is not allowed to read
  // r2.lifefarmer.ca without CORS in every build, and the http scope already allows https://*.
  fetch: async (url, init) => {
    const { fetch } = await import('@tauri-apps/plugin-http');
    return fetch(url, init);
  },
  openUrl: async (url) => (await import('@tauri-apps/plugin-opener')).openUrl(url),
  releasesUrl: RELEASES_URL,
};

let current: UpdaterPlugins = tauriPlugins;

export const updaterPlugins = (): UpdaterPlugins => current;

export const setUpdaterPlugins = (overrides: Partial<UpdaterPlugins>): void => {
  current = { ...current, ...overrides };
};

export const resetUpdaterPlugins = (): void => {
  current = tauriPlugins;
};
