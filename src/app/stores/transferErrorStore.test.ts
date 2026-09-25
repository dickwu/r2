import { beforeEach, describe, expect, test } from 'bun:test';
import type { TransferFailure } from '@/app/lib/transferFailure';
import { MAX_FAILURES, selectFocusedFailure, useTransferErrorStore } from './transferErrorStore';

const failure = (id: string, overrides: Partial<TransferFailure> = {}): TransferFailure => ({
  id,
  kind: 'upload',
  name: `${id}.bin`,
  message: 'AccessDenied: Access Denied',
  occurredAt: 1_000,
  ...overrides,
});

const state = () => useTransferErrorStore.getState();
const ids = () => state().failures.map((f) => f.id);

beforeEach(() => {
  useTransferErrorStore.setState({ isOpen: false, failures: [], focusedId: null });
});

describe('report', () => {
  test('opens on a new failure and focuses it', () => {
    state().report(failure('upload:1'));
    expect(state().isOpen).toBe(true);
    expect(state().focusedId).toBe('upload:1');
    expect(ids()).toEqual(['upload:1']);
  });

  test('a repeat with the same message stays closed and in place but records the new time', () => {
    state().report(failure('sync:a/b', { occurredAt: 1_000 }));
    state().report(failure('upload:1', { occurredAt: 2_000 }));
    state().close();
    state().report(failure('sync:a/b', { occurredAt: 61_000, stage: 'Listing' }));
    expect(state().isOpen).toBe(false);
    expect(ids()).toEqual(['sync:a/b', 'upload:1']);
    expect(state().failures[0].occurredAt).toBe(61_000);
    expect(state().failures[0].stage).toBe('Listing');
  });

  test('reopens when the message changed and makes that record the newest', () => {
    state().report(failure('sync:a/b'));
    state().report(failure('upload:1'));
    state().close();
    state().report(failure('sync:a/b', { message: 'no route to host', occurredAt: 2_000 }));
    expect(state().isOpen).toBe(true);
    expect(state().focusedId).toBe('sync:a/b');
    expect(ids()).toEqual(['upload:1', 'sync:a/b']);
    expect(state().failures[1].occurredAt).toBe(2_000);
  });

  test('an open modal stays on the failure being read while new ones arrive', () => {
    state().report(failure('upload:1'));
    state().report(failure('upload:2'));
    expect(state().isOpen).toBe(true);
    expect(state().focusedId).toBe('upload:1');
    expect(ids()).toEqual(['upload:1', 'upload:2']);
  });

  test('drops the oldest records past the cap', () => {
    for (let i = 0; i < MAX_FAILURES + 5; i += 1) {
      state().report(failure(`upload:${i}`));
      state().close();
    }
    expect(state().failures).toHaveLength(MAX_FAILURES);
    expect(ids()[0]).toBe('upload:5');
    expect(ids()[MAX_FAILURES - 1]).toBe(`upload:${MAX_FAILURES + 4}`);
  });

  test('never drops the failure being read to make room', () => {
    state().report(failure('upload:0'));
    for (let i = 1; i <= MAX_FAILURES; i += 1) state().report(failure(`upload:${i}`));
    expect(state().failures).toHaveLength(MAX_FAILURES);
    expect(state().focusedId).toBe('upload:0');
    expect(ids()[0]).toBe('upload:0');
    expect(ids()).not.toContain('upload:1');
    expect(ids()[MAX_FAILURES - 1]).toBe(`upload:${MAX_FAILURES}`);
  });
});

describe('show', () => {
  test('always opens and focuses, keeping the original occurredAt and position', () => {
    state().report(failure('upload:1', { occurredAt: 1_000 }));
    state().report(failure('upload:2'));
    state().close();
    state().show(failure('upload:1', { occurredAt: 9_000 }));
    expect(state().isOpen).toBe(true);
    expect(state().focusedId).toBe('upload:1');
    expect(ids()).toEqual(['upload:1', 'upload:2']);
    expect(state().failures[0].occurredAt).toBe(1_000);
  });

  test('takes the new time and moves to newest when the message changed', () => {
    state().report(failure('upload:1', { occurredAt: 1_000 }));
    state().report(failure('upload:2'));
    state().close();
    state().show(failure('upload:1', { message: 'no route to host', occurredAt: 9_000 }));
    expect(state().isOpen).toBe(true);
    expect(state().focusedId).toBe('upload:1');
    expect(ids()).toEqual(['upload:2', 'upload:1']);
    expect(state().failures[1].occurredAt).toBe(9_000);
  });

  test('adds a failure the store has not seen, as the newest', () => {
    state().report(failure('upload:1'));
    state().close();
    state().show(failure('download:7', { kind: 'download', occurredAt: 5_000 }));
    expect(state().isOpen).toBe(true);
    expect(ids()).toEqual(['upload:1', 'download:7']);
    expect(selectFocusedFailure(state())?.occurredAt).toBe(5_000);
  });
});

describe('open, select, close, clear', () => {
  test('open(id) focuses that record and opens', () => {
    state().report(failure('upload:1'));
    state().report(failure('upload:2'));
    state().close();
    state().open('upload:2');
    expect(state().isOpen).toBe(true);
    expect(state().focusedId).toBe('upload:2');
  });

  test('open() without a known id keeps the focus, else takes the newest', () => {
    state().report(failure('upload:1'));
    state().report(failure('upload:2'));
    state().close();
    state().open('missing');
    expect(state().isOpen).toBe(true);
    expect(state().focusedId).toBe('upload:1');

    useTransferErrorStore.setState({ isOpen: false, focusedId: null });
    state().open();
    expect(state().isOpen).toBe(true);
    expect(state().focusedId).toBe('upload:2');
  });

  test('open() with no records has nothing to show and stays closed', () => {
    state().open('sync:a/b');
    expect(state().isOpen).toBe(false);
    expect(state().focusedId).toBeNull();
  });

  test('select focuses only a known record', () => {
    state().report(failure('upload:1'));
    state().report(failure('upload:2'));
    state().select('upload:2');
    expect(state().focusedId).toBe('upload:2');
    state().select('missing');
    expect(state().focusedId).toBe('upload:2');
  });

  test('close hides the modal and keeps the records', () => {
    state().report(failure('upload:1'));
    state().close();
    expect(state().isOpen).toBe(false);
    expect(ids()).toEqual(['upload:1']);
    state().open('upload:1');
    expect(state().isOpen).toBe(true);
  });

  test('clear empties the records and closes', () => {
    state().report(failure('upload:1'));
    state().report(failure('upload:2'));
    state().clear();
    expect(state().isOpen).toBe(false);
    expect(state().failures).toHaveLength(0);
    expect(state().focusedId).toBeNull();
  });
});

describe('selectFocusedFailure', () => {
  test('returns the focused record, else the last one, else null', () => {
    expect(selectFocusedFailure(state())).toBeNull();
    state().report(failure('upload:1'));
    state().report(failure('upload:2'));
    expect(selectFocusedFailure(state())?.id).toBe('upload:1');
    useTransferErrorStore.setState({ focusedId: null });
    expect(selectFocusedFailure(state())?.id).toBe('upload:2');
    useTransferErrorStore.setState({ focusedId: 'gone' });
    expect(selectFocusedFailure(state())?.id).toBe('upload:2');
  });
});

describe('immutability', () => {
  test('never mutates a previous failures array or its records', () => {
    state().report(failure('upload:1'));
    state().report(failure('upload:2'));
    const before = state().failures;
    const snapshot = before.map((f) => ({ ...f }));

    state().report(failure('upload:1', { stage: 'Part 2 of 3' }));
    state().report(failure('upload:2', { message: 'changed' }));
    state().show(failure('upload:3'));
    state().select('upload:3');
    state().close();
    state().clear();

    expect(before).toHaveLength(2);
    expect(before).toEqual(snapshot);
  });

  test('a changed list is a new array', () => {
    state().report(failure('upload:1'));
    const before = state().failures;
    state().show(failure('upload:2'));
    expect(state().failures).not.toBe(before);
    expect(before).toHaveLength(1);
  });
});

describe('forget', () => {
  test('drops the record and moves the focus to the newest one left', () => {
    state().report(failure('upload:1'));
    state().report(failure('download:2'));
    state().select('upload:1');
    state().forget('upload:1');
    expect(ids()).toEqual(['download:2']);
    expect(state().focusedId).toBe('download:2');
    expect(state().isOpen).toBe(true);
  });

  test('closes an open modal whose last record is forgotten', () => {
    state().report(failure('upload:1'));
    state().forget('upload:1');
    expect(ids()).toEqual([]);
    expect(state().focusedId).toBeNull();
    expect(state().isOpen).toBe(false);
  });

  test('an unknown id changes nothing', () => {
    state().report(failure('upload:1'));
    const before = state();
    state().forget('nope');
    expect(state().failures).toBe(before.failures);
    expect(state().focusedId).toBe('upload:1');
  });

  test('makes the same failure news again: a forgotten record reopens on the next report', () => {
    state().report(failure('download:1'));
    state().close();
    state().report(failure('download:1'));
    expect(state().isOpen).toBe(false);
    state().forget('download:1');
    state().report(failure('download:1'));
    expect(state().isOpen).toBe(true);
    expect(ids()).toEqual(['download:1']);
  });
});
