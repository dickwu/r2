import { describe, expect, test } from 'bun:test';
import {
  classifySaveError,
  describeSaveError,
  etagsDiffer,
  normalizeEtag,
  saveWithVersionCheck,
  type SaveDependencies,
} from './saveFlow';

/** A fake bucket holding one object version, with the calls the save made. */
function server(etag: string | null, behaviour: { failWith?: string[]; noEtag?: boolean } = {}) {
  const state = {
    etag,
    uploads: [] as (string | undefined)[],
    failures: [...(behaviour.failWith ?? [])],
  };
  const deps: SaveDependencies = {
    currentEtag: async () => (behaviour.noEtag ? null : state.etag),
    upload: async (ifMatch) => {
      state.uploads.push(ifMatch);
      const failure = state.failures.shift();
      if (failure) throw new Error(failure);
      if (ifMatch && state.etag && normalizeEtag(ifMatch) !== normalizeEtag(state.etag)) {
        throw new Error('HTTP 412: PreconditionFailed: At least one of the pre-conditions failed');
      }
      state.etag = `"v${state.uploads.length + 1}"`;
      return state.etag;
    },
  };
  return { state, deps };
}

describe('etags', () => {
  test('normalizeEtag drops quotes and the weak marker', () => {
    expect(normalizeEtag('"abc"')).toBe('abc');
    expect(normalizeEtag('W/"abc"')).toBe('abc');
    expect(normalizeEtag('  ')).toBeNull();
    expect(normalizeEtag(null)).toBeNull();
  });

  test('etagsDiffer only reports a difference both sides know about', () => {
    expect(etagsDiffer('"a"', 'a')).toBe(false);
    expect(etagsDiffer('"a"', '"b"')).toBe(true);
    expect(etagsDiffer(null, '"b"')).toBe(false);
  });
});

describe('classifySaveError', () => {
  test('sorts backend errors by their status', () => {
    expect(classifySaveError(new Error('HTTP 412: PreconditionFailed'))).toBe('conflict');
    expect(classifySaveError('HTTP 409: ConditionalRequestConflict')).toBe('conflict');
    expect(classifySaveError(new Error('HTTP 403: AccessDenied: Access Denied'))).toBe('forbidden');
    expect(classifySaveError(new Error('HTTP 501: NotImplemented'))).toBe('condition-unsupported');
    expect(classifySaveError(new Error('HTTP 413: EntityTooLarge'))).toBe('too-large');
    expect(classifySaveError(new Error('dns error'))).toBe('other');
  });

  test('a read-only key gets a message saying the edits are kept', () => {
    expect(describeSaveError('forbidden', 'x')).toContain('read-only');
    expect(describeSaveError('other', 'socket closed')).toBe('socket closed');
  });
});

describe('saveWithVersionCheck', () => {
  test('saves over the version it opened, conditionally', async () => {
    const { state, deps } = server('"v1"');
    const outcome = await saveWithVersionCheck('"v1"', deps);
    expect(outcome.status).toBe('saved');
    expect(outcome.status === 'saved' && outcome.etag).toBe('v2');
    expect(state.uploads).toEqual(['"v1"']);
  });

  test('a newer version on the server is a conflict, and nothing is written', async () => {
    const { state, deps } = server('"v7"');
    const outcome = await saveWithVersionCheck('"v1"', deps);
    expect(outcome.status).toBe('conflict');
    expect(outcome.status === 'conflict' && outcome.serverEtag).toBe('"v7"');
    expect(state.uploads).toHaveLength(0);
  });

  test('a write that lands between the check and the PUT is caught by If-Match', async () => {
    const { state, deps } = server('"v1"');
    const racing: SaveDependencies = {
      currentEtag: deps.currentEtag,
      upload: async (ifMatch) => {
        state.etag = '"someone-else"';
        return deps.upload(ifMatch);
      },
    };
    expect((await saveWithVersionCheck('"v1"', racing)).status).toBe('conflict');
  });

  test('force overwrites without a condition', async () => {
    const { state, deps } = server('"v7"');
    const outcome = await saveWithVersionCheck('"v1"', deps, { force: true });
    expect(outcome.status).toBe('saved');
    expect(state.uploads).toEqual([undefined]);
  });

  test('an object opened without an ETag is saved unconditionally', async () => {
    const { state, deps } = server('"v1"');
    expect((await saveWithVersionCheck(null, deps)).status).toBe('saved');
    expect(state.uploads).toEqual([undefined]);
  });

  test('a server without conditional writes gets a plain PUT after the check passed', async () => {
    const { state, deps } = server('"v1"', { failWith: ['HTTP 501: NotImplemented'] });
    expect((await saveWithVersionCheck('"v1"', deps)).status).toBe('saved');
    expect(state.uploads).toEqual(['"v1"', undefined]);
  });

  test('a read-only key reports forbidden and keeps the edits', async () => {
    const { deps } = server('"v1"', { failWith: ['HTTP 403: AccessDenied'] });
    const outcome = await saveWithVersionCheck('"v1"', deps);
    expect(outcome.status).toBe('error');
    expect(outcome.status === 'error' && outcome.kind).toBe('forbidden');
  });

  test('a failed version check still saves under If-Match', async () => {
    const { state, deps } = server('"v1"');
    const flaky: SaveDependencies = {
      currentEtag: async () => {
        throw new Error('timeout');
      },
      upload: deps.upload,
    };
    expect((await saveWithVersionCheck('"v1"', flaky)).status).toBe('saved');
    expect(state.uploads).toEqual(['"v1"']);
  });
});
