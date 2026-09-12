import assert from 'node:assert/strict';
import { expect, test } from 'bun:test';
import { buildMoveOperations } from './moveOperations';

test('batch moves explicitly preserve existing destination names', () => {
  expect(buildMoveOperations(['a/x', 'b/y'], '/target/')).toEqual([
    { source_key: 'a/x', dest_key: 'target/x', overwrite: false },
    { source_key: 'b/y', dest_key: 'target/y', overwrite: false },
  ]);
});

test('flattened duplicate destination names are rejected before queuing', () => {
  assert.throws(() => buildMoveOperations(['a/x', 'b/x'], 'target'), /Multiple selected files/);
});
