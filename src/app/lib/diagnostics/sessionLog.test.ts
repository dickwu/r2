import { beforeEach, describe, expect, test } from 'bun:test';
import {
  clearSessionLog,
  logSession,
  recentSessionLog,
  SESSION_LOG_WINDOW_MINUTES,
} from './sessionLog';

const T0 = 1_756_800_000_000;
const MINUTE = 60_000;

describe('sessionLog', () => {
  beforeEach(() => clearSessionLog());

  test('records lines with whitespace collapsed and secrets redacted', () => {
    logSession('console', 'error', '  Upload   failed\n for key AKIAIOSFODNN7EXAMPLE ', T0);

    expect(recentSessionLog(T0).entries).toEqual([
      { ts: T0, kind: 'console', level: 'error', msg: 'Upload failed for key [redacted-key-id]' },
    ]);
  });

  test('drops empty lines and truncates long ones', () => {
    logSession('console', 'warn', '   ', T0);
    logSession('console', 'warn', 'x'.repeat(400), T0);

    const { entries } = recentSessionLog(T0);
    expect(entries).toHaveLength(1);
    expect(entries[0].msg).toHaveLength(301);
    expect(entries[0].msg.endsWith('…')).toBe(true);
  });

  test('returns only the window before the snapshot, oldest first', () => {
    logSession('app', 'info', 'too old', T0 - (SESSION_LOG_WINDOW_MINUTES + 1) * MINUTE);
    logSession('app', 'info', 'inside', T0 - 2 * MINUTE);
    logSession('console', 'error', 'latest', T0);

    const snapshot = recentSessionLog(T0);
    expect(snapshot.entries.map((entry) => entry.msg)).toEqual(['inside', 'latest']);
    expect(snapshot.capturedTs).toBe(T0);
    expect(snapshot.windowMinutes).toBe(SESSION_LOG_WINDOW_MINUTES);
    expect(snapshot.truncated).toBe(false);
  });

  test('a full ring inside the window reports itself truncated', () => {
    for (let i = 0; i < 250; i++) {
      logSession('console', 'warn', `line ${i}`, T0 - 1000 + i);
    }

    const snapshot = recentSessionLog(T0);
    expect(snapshot.entries).toHaveLength(200);
    expect(snapshot.entries[0].msg).toBe('line 50');
    expect(snapshot.truncated).toBe(true);
  });

  test('a ring that merely filled up, or overflowed outside the window, is not truncated', () => {
    for (let i = 0; i < 200; i++) {
      logSession('console', 'warn', `line `, T0 - 1000 + i);
    }
    expect(recentSessionLog(T0).truncated).toBe(false);

    clearSessionLog();
    for (let i = 0; i < 250; i++) {
      logSession('console', 'warn', `old `, T0 - 10 * MINUTE + i);
    }
    const snapshot = recentSessionLog(T0);
    expect(snapshot.entries).toHaveLength(0);
    expect(snapshot.truncated).toBe(false);
  });

  test('taking a snapshot does not consume the buffer', () => {
    logSession('app', 'info', 'kept', T0);
    recentSessionLog(T0);

    expect(recentSessionLog(T0).entries).toHaveLength(1);
  });
});
