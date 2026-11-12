import assert from 'node:assert/strict';
import { describe, expect, test } from 'bun:test';
import { QueryClient } from '@tanstack/react-query';
import { loadFolderItems, type FolderSnapshot } from '@/app/utils/folderItems';

const snap = (bucket: string): FolderSnapshot => ({
  items: [{ key: `${bucket}.txt`, name: `${bucket}.txt`, isFolder: false }],
  complete: true,
  fromCache: false,
  freshness: 'fresh',
});

describe('switching buckets while a listing is in flight', () => {
  test('cancelling bucket A leaves B as the only settled listing, even if A finishes later', async () => {
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    });
    const keyA = ['folder-contents', 'r2', 'acct', 'bucket-a', '', 'ns'];
    const keyB = ['folder-contents', 'r2', 'acct', 'bucket-b', '', 'ns'];
    const snapA = snap('bucket-a');
    const snapB = snap('bucket-b');
    let finishA!: (value: FolderSnapshot) => void;
    const listingA = new Promise<FolderSnapshot>((resolve) => {
      finishA = resolve;
    });

    const pendingA = queryClient.fetchQuery({
      queryKey: keyA,
      queryFn: async ({ signal }) =>
        loadFolderItems({
          config: { bucket: 'bucket-a' },
          prefix: '',
          signal,
          readCachedFolder: async () => null,
          readPrefixFolder: async () => listingA,
          onUpdate: (snapshot) => {
            if (!signal.aborted) queryClient.setQueryData(keyA, snapshot);
          },
        }),
    });

    // Same cleanup useR2Files runs when the observed folder key changes.
    await queryClient.cancelQueries({ queryKey: keyA });

    const resultB = await queryClient.fetchQuery({
      queryKey: keyB,
      queryFn: async ({ signal }) =>
        loadFolderItems({
          config: { bucket: 'bucket-b' },
          prefix: '',
          signal,
          readCachedFolder: async () => null,
          readPrefixFolder: async (_config, _prefix, options) => {
            options.onUpdate(snapB);
            return snapB;
          },
          onUpdate: (snapshot) => {
            if (!signal.aborted) queryClient.setQueryData(keyB, snapshot);
          },
        }),
    });

    finishA(snapA);
    await assert.rejects(pendingA);
    expect(resultB).toEqual(snapB);
    expect(queryClient.getQueryData(keyB)).toEqual(snapB);
    expect(queryClient.getQueryData(keyA)).toBeUndefined();
  });
});
