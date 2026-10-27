import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import {
  resetUpdaterPlugins,
  setUpdaterPlugins,
  type AvailableUpdate,
  type DownloadEvent,
} from '@/app/lib/updates/updaterPlugins';
import { setPrefStorage, type PrefStorage } from '@/app/lib/updates/updatePrefs';
import type { FetchLike } from '@/app/lib/updates/releaseNotes';
import { resetUpdateStoreForTests, updateNotes, useUpdateStore } from './updateStore';

const state = () => useUpdateStore.getState();

const prefs = new Map<string, string>();
const storage: PrefStorage = {
  getItem: (k) => prefs.get(k) ?? null,
  setItem: (k, v) => void prefs.set(k, v),
  removeItem: (k) => void prefs.delete(k),
};

const feedJson = {
  releases: [
    {
      version: '0.3.18',
      date: null,
      draft: true,
      title: 'Updates',
      summary: 's',
      notes: { new: ['Notes in the dialog.'] },
    },
    {
      version: '0.3.17',
      date: '2026-10-26',
      title: 'Editor',
      summary: 's',
      notes: { new: ['Editor.'] },
    },
  ],
};
const feedFetch: FetchLike = async () => ({ ok: true, status: 200, json: async () => feedJson });

interface Fake {
  update: AvailableUpdate;
  downloads: number;
  finish(): void;
  fail(error: unknown): void;
  emit(event: DownloadEvent): void;
}

/** An update whose download runs until the test finishes or fails it. */
const fakeUpdate = (version = '0.3.18', body?: string): Fake => {
  let resolve: () => void = () => {};
  let reject: (e: unknown) => void = () => {};
  let onEvent: ((event: DownloadEvent) => void) | undefined;
  const fake: Fake = {
    downloads: 0,
    update: {
      version,
      body,
      downloadAndInstall: (cb) => {
        fake.downloads += 1;
        onEvent = cb;
        return new Promise<void>((res, rej) => {
          resolve = res;
          reject = rej;
        });
      },
    },
    finish: () => {
      onEvent?.({ event: 'Finished' });
      resolve();
    },
    fail: (e) => reject(e),
    emit: (event) => onEvent?.(event),
  };
  return fake;
};

const tick = () => new Promise((r) => setTimeout(r, 0));

let relaunches = 0;

const usePlugins = (found: AvailableUpdate | null | (() => never), version = '0.3.17') => {
  setUpdaterPlugins({
    getVersion: async () => version,
    check: async () => (typeof found === 'function' ? found() : found),
    relaunch: async () => void (relaunches += 1),
    fetch: feedFetch,
    releasesUrl: 'https://feed.test/releases.json',
  });
};

describe('updateStore', () => {
  beforeEach(() => {
    prefs.clear();
    setPrefStorage(storage);
    resetUpdateStoreForTests();
    relaunches = 0;
  });
  afterEach(() => {
    resetUpdaterPlugins();
    setPrefStorage(null);
  });

  test('a background check opens the dialog once per version', async () => {
    usePlugins(fakeUpdate().update);
    await state().checkForUpdates();
    expect(state().phase).toBe('available');
    expect(state().dialogOpen).toBe(true);

    state().closeDialog();
    await state().checkForUpdates();
    expect(state().phase).toBe('available');
    expect(state().dialogOpen).toBe(false);

    // A check you ask for always shows it.
    await state().checkForUpdates({ manual: true });
    expect(state().dialogOpen).toBe(true);
  });

  test('Skip this version keeps background checks quiet for it', async () => {
    usePlugins(fakeUpdate().update);
    await state().checkForUpdates({ manual: true });
    state().skipThisVersion();
    resetUpdateStoreForTests();
    usePlugins(fakeUpdate().update);
    await state().checkForUpdates();
    expect(state().dialogOpen).toBe(false);
    expect(state().update?.version).toBe('0.3.18');
  });

  test('no update: a check you asked for says up to date, a background one says nothing', async () => {
    usePlugins(null);
    await state().checkForUpdates();
    expect(state().phase).toBe('idle');
    await state().checkForUpdates({ manual: true });
    expect(state().phase).toBe('upToDate');
    state().clearCheckResult();
    expect(state().phase).toBe('idle');
  });

  test('a failed check surfaces only when you asked for it', async () => {
    usePlugins(() => {
      throw new Error('error sending request: dns error');
    });
    await state().checkForUpdates();
    expect(state().phase).toBe('idle');
    expect(state().failure).toBeNull();
    await state().checkForUpdates({ manual: true });
    expect(state().phase).toBe('error');
    expect(state().failure?.kind).toBe('network');
  });

  test('download progress, double clicks, and Update and restart', async () => {
    const fake = fakeUpdate();
    usePlugins(fake.update);
    await state().checkForUpdates();
    const run = state().install({ restart: true });
    void state().install({ restart: true }); // double click
    expect(fake.downloads).toBe(1);
    expect(state().phase).toBe('downloading');

    fake.emit({ event: 'Started', data: { contentLength: 1000 } });
    fake.emit({ event: 'Progress', data: { chunkLength: 430 } });
    expect(state().progress).toEqual({ downloaded: 430, total: 1000 });

    fake.finish();
    await run;
    expect(relaunches).toBe(1);
    expect(state().phase).toBe('restarting');
  });

  test('Install now, restart later waits for Restart now', async () => {
    const fake = fakeUpdate();
    usePlugins(fake.update);
    await state().checkForUpdates();
    const run = state().install({ restart: false });
    fake.finish();
    await run;
    expect(state().phase).toBe('readyToRestart');
    expect(relaunches).toBe(0);
    // Checks don't undo an installed update.
    await state().checkForUpdates();
    expect(state().phase).toBe('readyToRestart');
    await state().restartNow();
    expect(relaunches).toBe(1);
  });

  test('a failed download explains itself, and Try again downloads again', async () => {
    const fake = fakeUpdate();
    usePlugins(fake.update);
    await state().checkForUpdates();
    const first = state().install({ restart: true });
    fake.fail(new Error('connection reset by peer'));
    await first;
    expect(state().phase).toBe('error');
    expect(state().failure).toMatchObject({ step: 'download', kind: 'network', retryable: true });

    // A background check doesn't paper over it.
    await state().checkForUpdates();
    expect(state().phase).toBe('error');

    const second = state().retry();
    await tick();
    expect(fake.downloads).toBe(2);
    expect(state().phase).toBe('downloading');
    fake.finish();
    await second;
    expect(relaunches).toBe(1);
  });

  test('a failure after Finished is an install failure', async () => {
    const fake = fakeUpdate();
    usePlugins(fake.update);
    await state().checkForUpdates();
    const run = state().install({ restart: true });
    fake.emit({ event: 'Finished' });
    expect(state().phase).toBe('installing');
    fake.fail('Permission denied (os error 13)');
    await run;
    expect(state().failure).toMatchObject({
      step: 'install',
      kind: 'permission',
      retryable: false,
    });
    state().closeDialog();
    expect(state().phase).toBe('available');
  });

  test("What's new after an upgrade, and the feed's notes for the dialog", async () => {
    usePlugins(null, '0.3.17');
    await state().init();
    expect(state().whatsNew).toBeNull(); // fresh install

    resetUpdateStoreForTests();
    usePlugins(fakeUpdate('0.3.18', 'Body from latest.json').update, '0.3.18');
    await state().init();
    expect(state().whatsNew).toEqual({ from: '0.3.17', to: '0.3.18' });
    await state().loadFeed();
    expect(state().feedStatus).toBe('ready');
  });

  test('dialog notes: the feed, then the update body, then nothing', async () => {
    usePlugins(null);
    const feed = await state().loadFeed();
    const update = { version: '0.3.18', body: 'From latest.json' };
    expect(updateNotes(feed, '0.3.17', update)).toEqual({
      source: 'feed',
      markdown: '## New\n\n- Notes in the dialog.',
    });
    expect(updateNotes(feed, '0.3.16', update).markdown).toContain('## v0.3.17: Editor');
    expect(updateNotes(null, '0.3.17', update)).toEqual({
      source: 'body',
      markdown: 'From latest.json',
    });
    expect(updateNotes(null, '0.3.17', { version: '0.3.18' }).source).toBe('none');
  });
});
