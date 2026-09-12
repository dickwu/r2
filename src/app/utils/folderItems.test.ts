import assert from 'node:assert/strict';
import { describe, expect, test } from 'bun:test';
import {
  createFolderPageAccumulator,
  loadFolderItems,
  type FolderSnapshot,
  type FolderPage,
  type FolderRequestScope,
} from './folderItems';

const scope: FolderRequestScope = {
  provider: 'r2',
  account_id: 'acct',
  bucket: 'b',
  prefix: '',
  request_id: 'one',
  generation: 1,
};
const page = (overrides: Partial<FolderPage> = {}): FolderPage => ({
  ...scope,
  files: [],
  folders: [],
  from_cache: false,
  freshness: 'fresh',
  complete: true,
  page_index: 0,
  next_cursor: null,
  ...overrides,
});
const emptyCache: FolderSnapshot = {
  items: [],
  complete: true,
  fromCache: true,
  freshness: 'stale',
};

describe('folder stale while revalidate', () => {
  test('publishes valid empty cache before the network resolves and propagates refresh errors', async () => {
    const updates: FolderSnapshot[] = [];
    let published!: () => void;
    const cachePublished = new Promise<void>((resolve) => {
      published = resolve;
    });
    let fail!: (error: Error) => void;
    const network = new Promise<FolderSnapshot>((_resolve, reject) => {
      fail = reject;
    });
    const pending = loadFolderItems({
      config: {},
      prefix: '',
      readCachedFolder: async () => emptyCache,
      readPrefixFolder: async () => network,
      onUpdate: (snapshot) => {
        updates.push(snapshot);
        published();
      },
    });
    await cachePublished;
    expect(updates).toEqual([emptyCache]);
    fail(new Error('offline'));
    await assert.rejects(pending, /offline/);
    expect(updates).toEqual([emptyCache]);
  });

  test('missing cache and failed listing reject instead of becoming an empty directory', async () => {
    await assert.rejects(
      loadFolderItems({
        config: {},
        prefix: '',
        readCachedFolder: async () => null,
        readPrefixFolder: async () => {
          throw new Error('denied');
        },
        onUpdate: () => {
          throw new Error('must not publish missing cache');
        },
      }),
      /denied/
    );
  });

  test('cache database error does not prevent a successful live listing', async () => {
    const live = { ...emptyCache, fromCache: false, freshness: 'fresh' as const };
    const result = await loadFolderItems({
      config: {},
      prefix: '',
      readCachedFolder: async () => {
        throw new Error('cache unavailable');
      },
      readPrefixFolder: async () => live,
      onUpdate: () => {},
    });
    expect(result).toEqual(live);
  });

  test('cancel during cache read never starts the stale navigation request', async () => {
    const controller = new AbortController();
    let started = false;
    const pending = loadFolderItems({
      config: {},
      prefix: '',
      signal: controller.signal,
      readCachedFolder: async () => {
        controller.abort();
        return emptyCache;
      },
      readPrefixFolder: async () => {
        started = true;
        return emptyCache;
      },
      onUpdate: () => {
        throw new Error('must not publish after cancellation');
      },
    });
    await assert.rejects(pending);
    expect(started).toBe(false);
  });
});

describe('scoped incremental folder pages', () => {
  test('first page is usable before final page, merges by key with natural folder-first sorting', () => {
    const accumulator = createFolderPageAccumulator(scope);
    const first = accumulator.accept(
      page({
        files: [{ key: 'file10', name: 'file10', size: 1, last_modified: '' }],
        complete: false,
        next_cursor: 'next',
      })
    );
    expect(first?.items.map((item) => item.key)).toEqual(['file10']);
    expect(first?.complete).toBe(false);
    const last = accumulator.accept(
      page({
        page_index: 1,
        folders: ['z/'],
        files: [
          { key: 'file2', name: 'file2', size: 2, last_modified: '' },
          { key: 'file10', name: 'file10', size: 3, last_modified: '' },
        ],
      })
    );
    expect(last?.items.map((item) => item.key)).toEqual(['z/', 'file2', 'file10']);
    expect(last?.items[2].size).toBe(3);
    expect(last?.complete).toBe(true);
  });

  test('ignores every mismatched scope field and obsolete request generation', () => {
    for (const field of [
      'provider',
      'account_id',
      'bucket',
      'prefix',
      'request_id',
      'generation',
    ] as const) {
      const accumulator = createFolderPageAccumulator(scope);
      expect(
        accumulator.accept(page({ [field]: field === 'generation' ? 2 : 'other' }))
      ).toBeNull();
      expect(accumulator.accept(page())?.complete).toBe(true);
    }
  });

  test('rejects missing or repeated cursors, skipped pages, and pages after completion', () => {
    assert.throws(
      () => createFolderPageAccumulator(scope).accept(page({ complete: false })),
      /cursor/
    );
    assert.throws(
      () => createFolderPageAccumulator(scope).accept(page({ page_index: 1 })),
      /sequence/
    );
    const accumulator = createFolderPageAccumulator(scope);
    accumulator.accept(page({ complete: false, next_cursor: 'same' }));
    assert.throws(
      () => accumulator.accept(page({ page_index: 1, complete: false, next_cursor: 'same' })),
      /cursor/
    );
    const finished = createFolderPageAccumulator(scope);
    finished.accept(page());
    assert.throws(() => finished.accept(page({ page_index: 1 })), /complete/);
  });
});
