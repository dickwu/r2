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
