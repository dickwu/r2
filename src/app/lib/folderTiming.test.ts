import assert from 'node:assert/strict';
import { afterEach, expect, test } from 'bun:test';
import { observeFolderAccumulation, observeFolderRequest } from './folderTiming';

const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
const scope = { provider: 'minio', account_id: 'fixture', bucket: 'photos', prefix: 'small/' };
afterEach(() => {
  if (originalWindow) Object.defineProperty(globalThis, 'window', originalWindow);
  else Reflect.deleteProperty(globalThis, 'window');
});

function installObserver(enabled: boolean, dispatch: (event: CustomEvent) => void) {
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: { __nativeListingAudit: enabled, dispatchEvent: dispatch },
  });
}

test('native listing observation preserves results and exposes counts instead of object data', async () => {
  const events: Record<string, unknown>[] = [];
  installObserver(true, (event) => events.push(event.detail));
  const result = {
    complete: true,
    from_cache: true,
    freshness: 'fresh',
    files: [{ key: 'private-name', secret_access_key: 'fixture-secret' }],
    folders: ['private-dir'],
    timing: { cache_ms: 3, shared_flight: false, secret: 'fixture-secret' },
  };
  const requestScope = { ...scope, secret_access_key: 'fixture-secret' };
  expect(await observeFolderRequest('get_prefix_cache', requestScope, async () => result)).toBe(
    result
  );
  expect(events.map((event) => event.phase)).toEqual(['start', 'complete']);
  expect(events[0].operation_id).toBe(events[1].operation_id);
  expect(events[1].result).toEqual({
    complete: true,
    from_cache: true,
    freshness: 'fresh',
    files: 1,
    folders: 1,
    timing: { cache_ms: 3, shared_flight: false },
  });
  expect(JSON.stringify(events)).not.toContain('private-name');
  expect(JSON.stringify(events)).not.toContain('fixture-secret');
});

test('disabled or broken observation cannot change request execution', async () => {
  for (const enabled of [false, true]) {
    let calls = 0;
    installObserver(enabled, () => {
      throw new Error('observer broke');
    });
    expect(
      await observeFolderRequest('cancel_prefix_list', scope, async () => {
        calls++;
        return 42;
      })
    ).toBe(42);
    expect(calls).toBe(1);
  }
});

test('observed failures retain the original rejection and redact credentials', async () => {
  const events: Record<string, unknown>[] = [];
  installObserver(true, (event) => events.push(event.detail));
  const error = new Error('secret_access_key=fixture-secret');
  const failure = observeFolderRequest('list_prefix_stream', scope, async () => {
    throw error;
  });
  await assert.rejects(failure, (caught) => caught === error);
  expect(events.map((event) => event.phase)).toEqual(['start', 'error']);
  expect(JSON.stringify(events)).not.toContain('fixture-secret');
});

test('accumulation timing exposes scoped elapsed time without changing values or errors', () => {
  const events: Record<string, unknown>[] = [];
  installObserver(true, (event) => events.push(event.detail));
  const value = { items: ['private-object-key'] };
  expect(
    observeFolderAccumulation({ ...scope, request_id: 'request', generation: 3 }, 2, () => value)
  ).toBe(value);
  expect(events[0].request_id).toBe('request');
  expect(events[0].page_index).toBe(2);
  expect(typeof events[0].duration_ms).toBe('number');
  expect(JSON.stringify(events)).not.toContain('private-object-key');
  const failure = new Error('merge failure');
  installObserver(true, () => {
    throw new Error('observer failure');
  });
  assert.throws(
    () =>
      observeFolderAccumulation(scope, 0, () => {
        throw failure;
      }),
    (caught) => caught === failure
  );
});
