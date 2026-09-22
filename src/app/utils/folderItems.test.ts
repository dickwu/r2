import assert from 'node:assert/strict';
import { describe, expect, test } from 'bun:test';
import {
  createFolderPageAccumulator,
  createFolderUpdatePublisher,
  buildFileItems,
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

describe('folder update frame publisher', () => {
  const snapshot = (key: string, complete = false): FolderSnapshot => ({
    items: [{ key, name: key, isFolder: false }],
    complete,
    fromCache: true,
    freshness: complete ? 'fresh' : 'partial',
  });

  function scheduler() {
    const callbacks: Array<() => void> = [];
    const cancelled = new Set<number>();
    return {
      callbacks,
      schedule: (callback: () => void) => {
        callbacks.push(callback);
        return callbacks.length - 1;
      },
      cancel: (handle: unknown) => cancelled.add(handle as number),
      run: () =>
        callbacks.splice(0).forEach((callback, index) => {
          if (!cancelled.has(index)) callback();
        }),
    };
  }

  test('publishes first update immediately and coalesces later partials to one frame', () => {
    const frames = scheduler();
    const updates: string[] = [];
    const publisher = createFolderUpdatePublisher((update) => updates.push(update.items[0].key), {
      scheduleFrame: frames.schedule,
      cancelFrame: frames.cancel,
    });
    publisher.publish(snapshot('first'));
    publisher.publish(snapshot('second'));
    publisher.publish(snapshot('third'));

    expect(updates).toEqual(['first']);
    frames.run();
    expect(updates).toEqual(['first', 'third']);
  });

  test('flushes pending partial before surfacing refresh errors', async () => {
    const frames = scheduler();
    const updates: string[] = [];
    await assert.rejects(
      loadFolderItems({
        config: {},
        prefix: '',
        scheduleFrame: frames.schedule,
        cancelFrame: frames.cancel,
        readCachedFolder: async () => null,
        readPrefixFolder: async (_config, _prefix, options) => {
          options.onUpdate(snapshot('first'));
          options.onUpdate(snapshot('pending'));
          throw new Error('offline');
        },
        onUpdate: (update) => updates.push(update.items[0].key),
      }),
      /offline/
    );
    expect(updates).toEqual(['first', 'pending']);
  });

  test('complete final update cancels pending partial and publishes immediately', () => {
    const frames = scheduler();
    const updates: string[] = [];
    const publisher = createFolderUpdatePublisher((update) => updates.push(update.items[0].key), {
      scheduleFrame: frames.schedule,
      cancelFrame: frames.cancel,
    });
    publisher.publish(snapshot('first'));
    publisher.publish(snapshot('pending'));
    publisher.publish(snapshot('final', true));
    frames.run();

    expect(updates).toEqual(['first', 'final']);
  });

  test('abort clears scheduled publication from obsolete generation', () => {
    const frames = scheduler();
    const controller = new AbortController();
    const updates: string[] = [];
    const publisher = createFolderUpdatePublisher((update) => updates.push(update.items[0].key), {
      signal: controller.signal,
      scheduleFrame: frames.schedule,
      cancelFrame: frames.cancel,
    });
    publisher.publish(snapshot('first'));
    publisher.publish(snapshot('pending'));
    controller.abort();
    frames.run();

    expect(updates).toEqual(['first']);
  });
});

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

  test('fresh complete cache satisfies normal navigation without live refresh', async () => {
    let started = false;
    const result = await loadFolderItems({
      config: {},
      prefix: '',
      readCachedFolder: async () => ({ ...emptyCache, freshness: 'fresh' }),
      readPrefixFolder: async () => {
        started = true;
        return emptyCache;
      },
      onUpdate: () => {},
    });
    expect(result).toEqual({ ...emptyCache, freshness: 'fresh' });
    expect(started).toBe(false);
  });

  test('manual refresh bypasses the fresh cache shortcut', async () => {
    let started = false;
    const live = { ...emptyCache, fromCache: false, freshness: 'fresh' as const };
    const result = await loadFolderItems({
      config: {},
      prefix: '',
      forceRefresh: true,
      readCachedFolder: async () => ({ ...emptyCache, freshness: 'fresh' }),
      readPrefixFolder: async (_config, _prefix, options) => {
        started = true;
        expect(options.forceRefresh).toBe(true);
        return live;
      },
      onUpdate: () => {},
    });
    expect(result).toEqual(live);
    expect(started).toBe(true);
  });

  test('manual refresh overlays live partial pages onto the current query snapshot', async () => {
    const fallback: FolderSnapshot = {
      items: [
        { key: 'a.txt', name: 'a.txt', isFolder: false, size: 1 },
        { key: 'z.txt', name: 'z.txt', isFolder: false, size: 9 },
      ],
      complete: true,
      fromCache: true,
      freshness: 'fresh',
    };
    const livePartial: FolderSnapshot = {
      items: [{ key: 'a.txt', name: 'a.txt', isFolder: false, size: 2 }],
      complete: false,
      fromCache: false,
      freshness: 'partial',
    };
    const updates: FolderSnapshot[] = [];
    await assert.rejects(
      loadFolderItems({
        config: {},
        prefix: '',
        forceRefresh: true,
        fallbackSnapshot: fallback,
        readCachedFolder: async () => {
          throw new Error('must not read cache');
        },
        readPrefixFolder: async (_config, _prefix, options) => {
          options.onUpdate(livePartial);
          throw new Error('offline');
        },
        onUpdate: (snapshot) => updates.push(snapshot),
      }),
      /offline/
    );
    expect(updates.map((snapshot) => snapshot.items.length)).toEqual([2]);
    expect(updates[0].items.map((item) => [item.key, item.size])).toEqual([
      ['a.txt', 2],
      ['z.txt', 9],
    ]);
  });

  test('warm cache paging overlays partial cache pages onto the current query snapshot', async () => {
    const fallback: FolderSnapshot = {
      items: [
        { key: 'a.txt', name: 'a.txt', isFolder: false, size: 1 },
        { key: 'z.txt', name: 'z.txt', isFolder: false, size: 9 },
      ],
      complete: true,
      fromCache: true,
      freshness: 'fresh',
    };
    const cachePartial: FolderSnapshot = {
      items: [{ key: 'a.txt', name: 'a.txt', isFolder: false, size: 2 }],
      complete: false,
      fromCache: true,
      freshness: 'partial',
    };
    const cacheComplete: FolderSnapshot = {
      items: [
        { key: 'a.txt', name: 'a.txt', isFolder: false, size: 2 },
        { key: 'z.txt', name: 'z.txt', isFolder: false, size: 9 },
      ],
      complete: true,
      fromCache: true,
      freshness: 'stale',
    };
    const updates: FolderSnapshot[] = [];
    await assert.rejects(
      loadFolderItems({
        config: {},
        prefix: '',
        fallbackSnapshot: fallback,
        readCachedFolder: async (_config, _prefix, options) => {
          options.onUpdate(cachePartial);
          return cacheComplete;
        },
        readPrefixFolder: async () => {
          throw new Error('offline');
        },
        onUpdate: (snapshot) => updates.push(snapshot),
      }),
      /offline/
    );
    expect(updates.map((snapshot) => snapshot.items.length)).toEqual([2, 2]);
    expect(updates[0].items.map((item) => [item.key, item.size])).toEqual([
      ['a.txt', 2],
      ['z.txt', 9],
    ]);
    expect(updates[1]).toEqual(cacheComplete);
  });

  test('warm partial refresh overlays verified rows without dropping unseen cached rows', async () => {
    const cached: FolderSnapshot = {
      items: [
        { key: 'a.txt', name: 'a.txt', isFolder: false, size: 1, lastModified: 'old' },
        { key: 'z.txt', name: 'z.txt', isFolder: false, size: 9, lastModified: 'old' },
      ],
      complete: true,
      fromCache: true,
      freshness: 'stale',
    };
    const livePartial: FolderSnapshot = {
      items: [{ key: 'a.txt', name: 'a.txt', isFolder: false, size: 2, lastModified: 'new' }],
      complete: false,
      fromCache: false,
      freshness: 'partial',
    };
    const updates: FolderSnapshot[] = [];
    await assert.rejects(
      loadFolderItems({
        config: {},
        prefix: '',
        readCachedFolder: async () => cached,
        readPrefixFolder: async (_config, _prefix, options) => {
          options.onUpdate(livePartial);
          throw new Error('offline');
        },
        onUpdate: (snapshot) => updates.push(snapshot),
      }),
      /offline/
    );
    expect(updates).toHaveLength(2);
    expect(updates[1].items.map((item) => [item.key, item.size])).toEqual([
      ['a.txt', 2],
      ['z.txt', 9],
    ]);
    expect(updates[1].complete).toBe(false);
    expect(updates[1].freshness).toBe('partial');
  });

  test('final warm refresh page can remove cached rows after full confirmation', async () => {
    const cached: FolderSnapshot = {
      items: [
        { key: 'a.txt', name: 'a.txt', isFolder: false, size: 1 },
        { key: 'deleted.txt', name: 'deleted.txt', isFolder: false, size: 2 },
      ],
      complete: true,
      fromCache: true,
      freshness: 'stale',
    };
    const liveComplete: FolderSnapshot = {
      items: [{ key: 'a.txt', name: 'a.txt', isFolder: false, size: 3 }],
      complete: true,
      fromCache: false,
      freshness: 'fresh',
    };
    const updates: FolderSnapshot[] = [];
    const result = await loadFolderItems({
      config: {},
      prefix: '',
      readCachedFolder: async () => cached,
      readPrefixFolder: async (_config, _prefix, options) => {
        options.onUpdate(liveComplete);
        return liveComplete;
      },
      onUpdate: (snapshot) => updates.push(snapshot),
    });
    expect(updates[1].items.map((item) => item.key)).toEqual(['a.txt']);
    expect(result.items.map((item) => item.key)).toEqual(['a.txt']);
  });
});

describe('scoped incremental folder pages', () => {
  test('lexical pages match the full natural stable sort with duplicate metadata and late folders', () => {
    const request = { ...scope, prefix: 'nested/' };
    const accumulator = createFolderPageAccumulator(request);
    const keys = Array.from({ length: 10_000 }, (_, index) => `nested/file-${index}.txt`).sort();
    const known = new Map<string, { key: string; size: number; lastModified: string }>();
    const knownFolders = new Set<string>();
    const retained: Array<{ snapshot: FolderSnapshot; json: string }> = [];
    for (let index = 0; index < 10; index++) {
      const entries = keys
        .slice(index * 1000, (index + 1) * 1000)
        .map((key) => ({ key, name: key, size: index, last_modified: `day-${index}` }));
      // Equal collator values retain first insertion order across pages.
      entries.push({
        key: `nested/tie-${index % 2 ? '01' : '1'}.txt`,
        name: 'ignored',
        size: index,
        last_modified: `day-${index}`,
      });
      if (index > 0)
        entries.push({
          key: keys[0],
          name: 'ignored',
          size: 9999 + index,
          last_modified: 'replaced',
        });
      const folders =
        index === 3
          ? ['nested/z/', 'nested/a01/', 'nested/a1/']
          : index === 8
            ? ['nested/a1/', 'nested/b/']
            : [];
      entries.forEach((file) =>
        known.set(file.key, { key: file.key, size: file.size, lastModified: file.last_modified })
      );
      folders.forEach((folder) => knownFolders.add(folder));
      const snapshot = accumulator.accept({
        ...page(),
        ...request,
        files: entries,
        folders,
        page_index: index,
        complete: index === 9,
        next_cursor: index === 9 ? null : String(index + 1),
      })!;
      expect(snapshot.items).toEqual(
        buildFileItems([...known.values()], [...knownFolders], request.prefix)
      );
      retained.forEach((older) => expect(JSON.stringify(older.snapshot)).toBe(older.json));
      retained.push({ snapshot, json: JSON.stringify(snapshot) });
    }
  });

  test('metadata replacements leave earlier snapshots and untouched item references intact', () => {
    const accumulator = createFolderPageAccumulator(scope);
    const first = accumulator.accept(
      page({
        files: [
          { key: 'a', name: 'ignored', size: 1, last_modified: 'old' },
          { key: 'b', name: 'ignored', size: 2, last_modified: 'unchanged' },
        ],
        complete: false,
        next_cursor: 'next',
      })
    )!;
    const second = accumulator.accept(
      page({
        page_index: 1,
        files: [
          { key: 'a', name: 'ignored', size: 10, last_modified: 'intermediate' },
          { key: 'a', name: 'ignored', size: 20, last_modified: 'new' },
        ],
      })
    )!;
    expect(first.items[0].size).toBe(1);
    expect(second.items[0].size).toBe(20);
    expect(second.items[1]).toBe(first.items[1]);
  });

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
