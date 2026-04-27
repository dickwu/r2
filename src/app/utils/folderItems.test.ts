import { describe, expect, test } from 'bun:test';
import { loadFolderItems, type FolderContents, type LazyPrefixResult } from './folderItems';

const staleCache: FolderContents = {
  folders: ['appointments/', 'common_documents/', 'documents/'],
  files: [{ key: '1.pdf', size: 1422625, lastModified: '2026-01-27T22:01:54.864Z' }],
};

const liveRoot: LazyPrefixResult = {
  prefix: '',
  from_cache: false,
  folders: [
    'appointments/',
    'backup/',
    'common_documents/',
    'documents/',
    'files/',
    'google-review-screenshots/',
    'insurance-cards/',
    'verification/',
  ],
  files: [
    { key: '1.pdf', name: '1.pdf', size: 1422625, last_modified: '2026-01-27T22:01:54.864Z' },
    { key: 'test.txt', name: 'test.txt', size: 24, last_modified: '2026-02-19T16:30:23.760Z' },
  ],
};

describe('loadFolderItems', () => {
  test('uses prefix listing even when SQLite already has partial cached rows', async () => {
    const calls: Array<{ prefix: string; forceRefresh?: boolean }> = [];

    const result = await loadFolderItems({
      config: { bucket: 'secret' },
      prefix: '',
      forceRefresh: true,
      readCachedFolder: async () => staleCache,
      readPrefixFolder: async (_config, prefix, options) => {
        calls.push({ prefix, forceRefresh: options?.forceRefresh });
        return liveRoot;
      },
    });

    expect(calls).toEqual([{ prefix: '', forceRefresh: true }]);
    expect(result.source).toBe('prefix');
    expect(result.items.map((item) => item.key)).toEqual([
      'appointments/',
      'backup/',
      'common_documents/',
      'documents/',
      'files/',
      'google-review-screenshots/',
      'insurance-cards/',
      'verification/',
      '1.pdf',
      'test.txt',
    ]);
  });

  test('passes force refresh to the prefix reader', async () => {
    const calls: Array<{ forceRefresh?: boolean }> = [];

    await loadFolderItems({
      config: { bucket: 'secret' },
      prefix: '',
      forceRefresh: true,
      readCachedFolder: async () => staleCache,
      readPrefixFolder: async (_config, _prefix, options) => {
        calls.push({ forceRefresh: options?.forceRefresh });
        return liveRoot;
      },
    });

    expect(calls).toEqual([{ forceRefresh: true }]);
  });

  test('falls back to SQLite cache when prefix listing fails', async () => {
    const result = await loadFolderItems({
      config: { bucket: 'secret' },
      prefix: '',
      readCachedFolder: async () => staleCache,
      readPrefixFolder: async () => {
        throw new Error('network unavailable');
      },
    });

    expect(result.source).toBe('cache-fallback');
    expect(result.items.map((item) => item.key)).toEqual([
      'appointments/',
      'common_documents/',
      'documents/',
      '1.pdf',
    ]);
  });

  test('reconstructs a folder from full cached files when exact prefix cache is empty', async () => {
    const result = await loadFolderItems({
      config: { bucket: 'secret' },
      prefix: '',
      readCachedFolder: async () => ({ files: [], folders: [] }),
      readAllCachedFiles: async () => [
        { key: '1.pdf', size: 1422625, lastModified: '2026-01-27T22:01:54.864Z' },
        { key: 'test.txt', size: 24, lastModified: '2026-02-19T16:30:23.760Z' },
        {
          key: 'appointments/1441909/istat/870095715388256257.png',
          size: 8042,
          lastModified: '2026-01-14T16:09:44.156Z',
        },
        {
          key: 'backup/export.zip',
          size: 100,
          lastModified: '2026-02-19T16:30:23.760Z',
        },
      ],
      readPrefixFolder: async () => {
        throw new Error('dispatch failure');
      },
    });

    expect(result.source).toBe('all-cache-fallback');
    expect(result.items.map((item) => item.key)).toEqual([
      'appointments/',
      'backup/',
      '1.pdf',
      'test.txt',
    ]);
  });
});
