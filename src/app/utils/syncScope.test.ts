import { describe, expect, test } from 'bun:test';
import {
  affectedMoveFolders,
  createFolderInvalidator,
  matchesSyncRun,
  type SyncScope,
} from './syncScope';

describe('sync task scope', () => {
  const scope: SyncScope = {
    provider: 'minio',
    account_id: 'a',
    bucket: 'b',
    prefix: '',
    run_id: 'new',
  };
  test('accepts only the active provider/account/bucket/prefix/run', () => {
    expect(matchesSyncRun(scope, scope)).toBe(true);
    expect(matchesSyncRun(null, scope)).toBe(false);
    for (const field of Object.keys(scope))
      expect(matchesSyncRun(scope, { ...scope, [field]: 'old' })).toBe(false);
  });
  test('move refreshes source/destination ancestors without touching unrelated buckets', () => {
    const task = {
      sourceProvider: 'r2',
      sourceAccountId: 'a',
      sourceBucket: 'source',
      sourceKey: 'folder/x',
      destProvider: 'aws',
      destAccountId: 'b',
      destBucket: 'target',
      destKey: 'other/y',
      deleteOriginal: true,
    };
    expect(affectedMoveFolders(task)).toEqual([
      ['folder-contents', 'r2', 'a', 'source', 'folder/'],
      ['folder-contents', 'r2', 'a', 'source', ''],
      ['folder-contents', 'aws', 'b', 'target', 'other/'],
      ['folder-contents', 'aws', 'b', 'target', ''],
    ]);
    expect(affectedMoveFolders({ ...task, deleteOriginal: false })).toHaveLength(2);
  });
  test('coalesces duplicate invalidations and cleanup discards queued work', async () => {
    const calls: string[][] = [];
    const invalidator = createFolderInvalidator((key) => calls.push(key), 5);
    invalidator.add([['one'], ['one']]);
    invalidator.add([['two'], ['one']]);
    await new Promise((resolve) => setTimeout(resolve, 15));
    expect(calls).toEqual([['one'], ['two']]);
    invalidator.add([['cancelled']]);
    invalidator.dispose();
    await new Promise((resolve) => setTimeout(resolve, 15));
    expect(calls).toHaveLength(2);
  });
});
