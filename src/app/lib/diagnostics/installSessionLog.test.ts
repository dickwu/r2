import { describe, expect, test } from 'bun:test';
import { describeConsoleArgs } from './installSessionLog';

describe('describeConsoleArgs', () => {
  test('joins strings, errors and objects into one line', () => {
    expect(
      describeConsoleArgs([
        'Sync failed:',
        new TypeError('boom'),
        { code: 500 },
        null,
        undefined,
        42,
      ])
    ).toBe('Sync failed: TypeError: boom {"code":500} null undefined 42');
  });

  test('never throws on values that cannot be printed', () => {
    const cyclic: Record<string, unknown> = {};
    cyclic.self = cyclic;

    expect(describeConsoleArgs([Symbol('s'), cyclic])).toBe('Symbol(s) [unprintable]');
  });

  test('redacts inside objects before cutting them, so no fragment of a secret survives', () => {
    // 150 characters of padding leave 31 characters of the secret before the
    // 200-character cut — one short of the hex rule's threshold if the cut
    // ran first.
    const straddling = { note: 'x'.repeat(150), blob: 'f'.repeat(64) };
    expect(describeConsoleArgs([straddling])).not.toContain('fff');

    const config = { secret_access_key: 'f'.repeat(64), endpoint: 'https://x' };
    expect(describeConsoleArgs(['save failed', config])).toBe(
      'save failed {"secret_access_key":"[redacted]","endpoint":"https://x"}'
    );
  });
});
