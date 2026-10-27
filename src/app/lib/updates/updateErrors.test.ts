import { describe, expect, test } from 'bun:test';
import { classifyUpdateError, describeUpdateError, shortFailure } from './updateErrors';

describe('classifyUpdateError', () => {
  const cases: Array<[string, string]> = [
    [
      'error sending request for url (https://github.com/dickwu/r2/releases/latest/download/latest.json)',
      'network',
    ],
    ['operation timed out', 'network'],
    ['signature verification failed', 'signature'],
    ['Permission denied (os error 13)', 'permission'],
    ['Access is denied. (os error 5)', 'permission'],
    ['No space left on device (os error 28)', 'disk'],
    ['Could not fetch a valid release JSON from the remote', 'server'],
    ['something odd', 'unknown'],
  ];
  for (const [detail, kind] of cases) {
    test(`${detail} is ${kind}`, () => {
      expect(classifyUpdateError(detail)).toBe(kind as ReturnType<typeof classifyUpdateError>);
    });
  }
});

describe('describeUpdateError', () => {
  test('a failed check says how to fix it and can be retried', () => {
    const failure = describeUpdateError('check', new Error('error sending request: dns error'));
    expect(failure.message).toBe(
      "Couldn't reach the update server. Check your internet connection, then try again."
    );
    expect(failure.retryable).toBe(true);
    expect(shortFailure(failure)).toBe("Couldn't check for updates");
  });

  test('a bad signature points to GitHub instead of retrying', () => {
    const failure = describeUpdateError('download', 'signature verification failed');
    expect(failure.retryable).toBe(false);
    expect(failure.message).toContain('Download the new version from GitHub');
    expect(shortFailure(failure)).toBe('Update failed');
  });

  test('keeps the raw text as detail, even for odd values', () => {
    expect(describeUpdateError('install', { code: 7 }).detail).toBe('{"code":7}');
    expect(describeUpdateError('install', '').detail).toBe('No details were given.');
    expect(describeUpdateError('install', 'weird').message).toBe(
      "The update didn't install. Try again, or download the new version from GitHub."
    );
  });
});
