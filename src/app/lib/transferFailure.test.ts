import { describe, expect, test } from 'bun:test';
import {
  KIND_LABEL,
  failurePercent,
  failureTitle,
  firstLine,
  formatCount,
  formatFailureAmount,
  formatFailureClock,
  formatFailureDetails,
  formatFailureTimestamp,
  isCountKind,
  splitErrorCode,
  type TransferFailure,
} from './transferFailure';
import { formatBytes } from '@/app/utils/formatBytes';

// Built from local wall-clock parts, so the expected strings hold in any time zone.
const AT = new Date(2026, 8, 25, 10, 42, 7).getTime();
const MB = 1024 * 1024;
// formatBytes renders these as "41.2 MB" and "66.5 MB" — the documented example.
const DONE = Math.round(41.2 * MB);
const TOTAL = Math.round(66.5 * MB);

const failure = (overrides: Partial<TransferFailure> = {}): TransferFailure => ({
  id: 'upload:1',
  kind: 'upload',
  name: 'IMG_2214.CR2',
  message: 'AccessDenied: The AWS Access Key Id you provided does not exist in our records.',
  occurredAt: AT,
  ...overrides,
});

describe('KIND_LABEL', () => {
  test('names every kind', () => {
    expect(KIND_LABEL).toEqual({
      upload: 'Upload',
      download: 'Download',
      move: 'Move',
      rename: 'Rename',
      sync: 'Sync',
      delete: 'Delete',
      mount: 'Mount transfer',
    });
  });
});

describe('splitErrorCode', () => {
  test('splits a provider code from its message', () => {
    expect(splitErrorCode('AccessDenied: Access Denied')).toEqual({
      code: 'AccessDenied',
      text: 'Access Denied',
    });
  });

  test('leaves a message without a leading PascalCase code whole', () => {
    for (const message of [
      'Upload failed: 403 - Forbidden',
      'transient: GET: try again',
      'no route to host; check the endpoint',
    ]) {
      expect(splitErrorCode(message)).toEqual({ code: null, text: message });
    }
  });

  test('keeps a multi-line remainder and trims around it', () => {
    expect(splitErrorCode('  NoSuchKey: The key does not exist.\nRequest ID: 42  \n')).toEqual({
      code: 'NoSuchKey',
      text: 'The key does not exist.\nRequest ID: 42',
    });
  });

  test('a message that is only a code keeps the code as text', () => {
    expect(splitErrorCode('AccessDenied')).toEqual({ code: null, text: 'AccessDenied' });
    expect(splitErrorCode('AccessDenied: ')).toEqual({ code: null, text: 'AccessDenied' });
  });

  test('needs a token of three or more characters directly followed by ": "', () => {
    expect(splitErrorCode('IO: broken pipe')).toEqual({ code: null, text: 'IO: broken pipe' });
    expect(splitErrorCode('AccessDenied:denied')).toEqual({
      code: null,
      text: 'AccessDenied:denied',
    });
  });
});

describe('failurePercent', () => {
  test('is null without progress or a usable total', () => {
    expect(failurePercent(undefined)).toBeNull();
    expect(failurePercent(null)).toBeNull();
    expect(failurePercent({ done: 5, total: 0 })).toBeNull();
    expect(failurePercent({ done: 5, total: -10 })).toBeNull();
    expect(failurePercent({ done: Number.NaN, total: 10 })).toBeNull();
    expect(failurePercent({ done: 5, total: Number.POSITIVE_INFINITY })).toBeNull();
  });

  test('rounds to a whole percent', () => {
    expect(failurePercent({ done: DONE, total: TOTAL })).toBe(62);
    expect(failurePercent({ done: 1, total: 3 })).toBe(33);
  });

  test('clamps to 0..100', () => {
    expect(failurePercent({ done: -5, total: 10 })).toBe(0);
    expect(failurePercent({ done: 15, total: 10 })).toBe(100);
    expect(failurePercent({ done: 10, total: 10 })).toBe(100);
  });

  test('never reads 100 for an unfinished transfer', () => {
    expect(failurePercent({ done: 999, total: 1000 })).toBe(99);
  });
});

describe('failureTitle', () => {
  test('names the kind for a single failure', () => {
    expect(failureTitle([failure()])).toBe('Upload failed');
    expect(failureTitle([failure({ kind: 'mount' })])).toBe('Mount transfer failed');
  });

  test("a record's own title wins for a single failure", () => {
    const note = failure({ kind: 'move', title: 'Move needs attention' });
    expect(failureTitle([note])).toBe('Move needs attention');
    expect(formatFailureDetails(note, String).startsWith('Move needs attention — ')).toBe(true);
  });

  test('counts several failures', () => {
    const several = [failure(), failure({ id: 'sync:a/b', kind: 'sync' }), failure({ id: 'x' })];
    expect(failureTitle(several)).toBe('3 failures');
  });
});

describe('formatFailureAmount', () => {
  test('formats bytes for transfers', () => {
    const upload = failure({ progress: { done: DONE, total: TOTAL } });
    expect(formatFailureAmount(upload, formatBytes)).toBe('41.2 MB of 66.5 MB');
  });

  test('counts items for delete and rename', () => {
    expect(isCountKind('delete')).toBe(true);
    expect(isCountKind('rename')).toBe(true);
    expect(isCountKind('upload')).toBe(false);
    const rename = failure({ kind: 'rename', progress: { done: 1200, total: 1500 } });
    expect(formatFailureAmount(rename, formatCount)).toBe('1,200 of 1,500 items');
    const single = failure({ kind: 'delete', progress: { done: 0, total: 1 } });
    expect(formatFailureAmount(single, formatCount)).toBe('0 of 1 item');
  });

  test('formats whole units when progress is fractional', () => {
    const fractional = failure({ progress: { done: 0.4, total: 1.6 } });
    expect(formatFailureAmount(fractional, formatBytes)).toBe('0 B of 2 B');
  });

  test('clamps done into range and is null without a usable total', () => {
    const over = failure({ progress: { done: 2 * MB, total: MB } });
    expect(formatFailureAmount(over, formatBytes)).toBe('1 MB of 1 MB');
    expect(
      formatFailureAmount(failure({ progress: { done: 1, total: 0 } }), formatBytes)
    ).toBeNull();
    expect(formatFailureAmount(failure(), formatBytes)).toBeNull();
  });
});

describe('firstLine', () => {
  test('returns the first non-empty line, trimmed', () => {
    expect(firstLine('\n  AccessDenied: no  \nRequest ID: 42')).toBe('AccessDenied: no');
    expect(firstLine('one line')).toBe('one line');
    expect(firstLine('   \n  ')).toBe('');
  });
});

describe('failure clock', () => {
  test('formats local time and blanks an unusable one', () => {
    expect(formatFailureClock(AT)).toBe('10:42:07');
    expect(formatFailureTimestamp(AT)).toBe('2026-09-25 10:42:07');
    expect(formatFailureClock(0)).toBe('');
    expect(formatFailureTimestamp(Number.NaN)).toBe('');
  });
});

describe('formatFailureDetails', () => {
  test('matches the documented layout', () => {
    const text = formatFailureDetails(
      failure({
        account: 'Greenwoods R2',
        bucket: 'media',
        key: 'photos/2026/IMG_2214.CR2',
        stage: 'Part 7 of 11',
        progress: { done: DONE, total: TOTAL },
      }),
      formatBytes
    );
    expect(text).toBe(
      [
        'Upload failed — IMG_2214.CR2',
        'Account: Greenwoods R2',
        'Bucket: media',
        'Key: photos/2026/IMG_2214.CR2',
        'Stage: Part 7 of 11',
        'Progress: 41.2 MB of 66.5 MB (62%)',
        'When: 2026-09-25 10:42:07',
        '',
        'AccessDenied: The AWS Access Key Id you provided does not exist in our records.',
      ].join('\n')
    );
  });

  test('omits every line whose value is missing', () => {
    const text = formatFailureDetails(
      failure({
        kind: 'sync',
        name: 'media',
        account: undefined,
        bucket: '',
        stage: '   ',
        progress: { done: 0, total: 0 },
        message: '  no route to host\n',
      }),
      formatBytes
    );
    expect(text).toBe(
      ['Sync failed — media', 'When: 2026-09-25 10:42:07', '', 'no route to host'].join('\n')
    );
  });

  test('drops the name from the heading and the message block when they are empty', () => {
    const text = formatFailureDetails(failure({ name: '', message: '' }), formatBytes);
    expect(text).toBe(['Upload failed', 'When: 2026-09-25 10:42:07'].join('\n'));
  });

  test('reads item counts for delete and rename', () => {
    const text = formatFailureDetails(
      failure({ kind: 'delete', name: '3 of 150 objects', progress: { done: 3, total: 150 } }),
      formatCount
    );
    expect(text).toContain('\nProgress: 3 of 150 items (2%)\n');
  });

  test('uses now for a record without a usable time', () => {
    const text = formatFailureDetails(failure({ occurredAt: 0 }), formatBytes, AT);
    expect(text).toContain('\nWhen: 2026-09-25 10:42:07\n');
  });
});
