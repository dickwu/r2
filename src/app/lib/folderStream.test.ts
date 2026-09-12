import assert from 'node:assert/strict';
import { describe, expect, test } from 'bun:test';
import { QueryClient, QueryObserver } from '@tanstack/react-query';
import { readFolderStream } from './folderStream';
import {
  loadFolderItems,
  type FolderPage,
  type FolderRequestScope,
  type FolderSnapshot,
} from '@/app/utils/folderItems';

const scope: FolderRequestScope = {
  provider: 'r2',
  account_id: 'a',
  bucket: 'b',
  prefix: '',
  request_id: 'r',
  generation: 1,
};
const page: FolderPage = {
  ...scope,
  files: [],
  folders: [],
  complete: true,
  from_cache: false,
  freshness: 'fresh',
  page_index: 0,
  next_cursor: null,
};

describe('folder stream lifecycle', () => {
  test('registers before invoke, ignores stale events and removes listener on success', async () => {
    let receive!: (page: FolderPage) => void;
    let removed = false;
    const snapshots: FolderSnapshot[] = [];
    const result = await readFolderStream(
      scope,
      {
        listen: async (callback) => {
          receive = callback;
          return () => {
            removed = true;
          };
        },
        start: async () => {
          receive({ ...page, request_id: 'old' });
          receive(page);
        },
        cancel: async () => {},
      },
      (snapshot) => snapshots.push(snapshot)
    );
    expect(snapshots).toHaveLength(1);
    expect(result.complete).toBe(true);
    expect(removed).toBe(true);
  });

  test('navigation abort cancels a pending native invoke and rejects late data', async () => {
    const controller = new AbortController();
    let receive!: (page: FolderPage) => void;
    let began!: () => void;
    const started = new Promise<void>((resolve) => {
      began = resolve;
    });
    let cancelled = 0;
    let removed = false;
    const snapshots: FolderSnapshot[] = [];
    const pending = readFolderStream(
      scope,
      {
        listen: async (callback) => {
          receive = callback;
          return () => {
            removed = true;
          };
        },
        start: () => {
          began();
          return new Promise(() => {});
        },
        cancel: async () => {
          cancelled += 1;
        },
      },
      (snapshot) => snapshots.push(snapshot),
      controller.signal
    );
    await started;
    controller.abort();
    await assert.rejects(pending);
    receive(page);
    expect(cancelled).toBe(1);
    expect(removed).toBe(true);
    expect(snapshots).toHaveLength(0);
  });

  test('a malformed later page preserves earlier partial rows and cancels native work', async () => {
    let receive!: (page: FolderPage) => void;
    let cancelled = false;
    const snapshots: FolderSnapshot[] = [];
    await assert.rejects(
      readFolderStream(
        scope,
        {
          listen: async (callback) => {
            receive = callback;
            return () => {};
          },
          start: async () => {
            receive({ ...page, complete: false, next_cursor: 'next', folders: ['a/'] });
            receive({ ...page, page_index: 2 });
          },
          cancel: async () => {
            cancelled = true;
          },
        },
        (snapshot) => snapshots.push(snapshot)
      ),
      /sequence/
    );
    expect(snapshots).toHaveLength(1);
    expect(snapshots[0].items[0].key).toBe('a/');
    expect(snapshots[0].complete).toBe(false);
    expect(cancelled).toBe(true);
  });

  test('cancellation during listener setup cleans up without invoking the backend', async () => {
    const controller = new AbortController();
    let installed!: (remove: () => void) => void;
    let removed = false;
    let started = false;
    const pending = readFolderStream(
      scope,
      {
        listen: () =>
          new Promise((resolve) => {
            installed = resolve;
          }),
        start: async () => {
          started = true;
        },
        cancel: async () => {},
      },
      () => {},
      controller.signal
    );
    controller.abort();
    installed(() => {
      removed = true;
    });
    await assert.rejects(pending);
    expect(started).toBe(false);
    expect(removed).toBe(true);
  });
});

test('React Query retains a valid empty cache while exposing the refresh error', async () => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  const key = ['folder-contents', 'r2', 'a', 'b', ''];
  const cached: FolderSnapshot = { items: [], complete: true, fromCache: true, freshness: 'stale' };
  const observer = new QueryObserver(client, { queryKey: key, enabled: false });
  const unsubscribe = observer.subscribe(() => {});
  await assert.rejects(
    client.fetchQuery({
      queryKey: key,
      queryFn: () =>
        loadFolderItems({
          config: {},
          prefix: '',
          readCachedFolder: async () => cached,
          readPrefixFolder: async () => {
            throw new Error('offline');
          },
          onUpdate: (snapshot) => client.setQueryData(key, snapshot),
        }),
    }),
    /offline/
  );
  expect(client.getQueryData(key)).toEqual(cached);
  expect(client.getQueryState(key)?.status).toBe('error');
  unsubscribe();
  client.clear();
});
