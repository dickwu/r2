import { describe, expect, test } from 'bun:test';
import { isProvisionalZeroNode, shouldReuseFolderMetadata } from './folderSizeStore';
import type { DirectoryNode } from '@/app/lib/r2cache';

function node(overrides: Partial<DirectoryNode>): DirectoryNode {
  return {
    path: overrides.path ?? 'documents/',
    fileCount: overrides.fileCount ?? 0,
    totalFileCount: overrides.totalFileCount ?? 0,
    size: overrides.size ?? 0,
    totalSize: overrides.totalSize ?? 0,
    lastModified: overrides.lastModified ?? null,
    lastUpdated: overrides.lastUpdated ?? 0,
  };
}

describe('isProvisionalZeroNode', () => {
  test('treats zero non-root folder aggregate as provisional', () => {
    expect(isProvisionalZeroNode('documents/', node({ path: 'documents/' }))).toBe(true);
  });

  test('trusts root zero aggregate', () => {
    expect(isProvisionalZeroNode('', node({ path: '' }))).toBe(false);
  });

  test('trusts nonzero folder aggregate', () => {
    expect(
      isProvisionalZeroNode(
        'documents/',
        node({ path: 'documents/', totalSize: 42, totalFileCount: 1 })
      )
    ).toBe(false);
  });
});

describe('shouldReuseFolderMetadata', () => {
  test('does not reuse zero fallback metadata with unknown counts', () => {
    expect(
      shouldReuseFolderMetadata('files/', {
        size: 0,
        fileCount: null,
        totalFileCount: null,
        lastModified: null,
      })
    ).toBe(false);
  });

  test('does not reuse nonzero fallback metadata with unknown counts', () => {
    expect(
      shouldReuseFolderMetadata('files/', {
        size: 100,
        fileCount: null,
        totalFileCount: null,
        lastModified: null,
      })
    ).toBe(false);
  });

  test('reuses nonzero folder metadata with known counts', () => {
    expect(
      shouldReuseFolderMetadata('files/', {
        size: 100,
        fileCount: 1,
        totalFileCount: 1,
        lastModified: null,
      })
    ).toBe(true);
  });

  test('does not reuse non-root zero-count metadata', () => {
    expect(
      shouldReuseFolderMetadata('files/', {
        size: 0,
        fileCount: 0,
        totalFileCount: 0,
        lastModified: null,
      })
    ).toBe(false);
  });

  test('reuses root zero-count metadata', () => {
    expect(
      shouldReuseFolderMetadata('', {
        size: 0,
        fileCount: 0,
        totalFileCount: 0,
        lastModified: null,
      })
    ).toBe(true);
  });
});

describe('folder metadata generation fence', () => {
  test('a late batch result cannot repopulate an account-cleared cache', async () => {
    const { spyOn } = await import('bun:test');
    const cache = await import('@/app/lib/r2cache');
    const { useFolderSizeStore } = await import('./folderSizeStore');
    let finish!: (value: (DirectoryNode | null)[]) => void;
    const delayed = new Promise<(DirectoryNode | null)[]>((resolve) => {
      finish = resolve;
    });
    const request = spyOn(cache, 'getDirectoryNodes').mockImplementation(() => delayed);
    try {
      useFolderSizeStore.getState().clearSizes();
      const pending = useFolderSizeStore.getState().loadMetadataList(['documents/']);
      useFolderSizeStore.getState().clearSizes();
      useFolderSizeStore.getState().setMetadata('documents/', {
        size: 9,
        fileCount: 1,
        totalFileCount: 1,
        lastModified: null,
      });
      finish([node({ totalSize: 100, totalFileCount: 1 })]);
      await pending;
      expect(useFolderSizeStore.getState().metadata['documents/']?.size).toBe(9);
    } finally {
      request.mockRestore();
      useFolderSizeStore.getState().clearSizes();
    }
  });

  test('a late size result and error cannot recreate cleared rows', async () => {
    const { spyOn } = await import('bun:test');
    const cache = await import('@/app/lib/r2cache');
    const { useFolderSizeStore } = await import('./folderSizeStore');
    for (const rejects of [false, true]) {
      let finish!: (value: number) => void;
      let fail!: (error: Error) => void;
      const delayed = new Promise<number>((resolve, reject) => {
        finish = resolve;
        fail = reject;
      });
      const request = spyOn(cache, 'calculateFolderSize').mockImplementation(() => delayed);
      try {
        const pending = useFolderSizeStore.getState().calculateSize('documents/');
        useFolderSizeStore.getState().clearSizes();
        if (rejects) fail(new Error('late failure'));
        else finish(99);
        await pending;
        expect(useFolderSizeStore.getState().sizes).toEqual({});
      } finally {
        request.mockRestore();
        useFolderSizeStore.getState().clearSizes();
      }
    }
  });
});
