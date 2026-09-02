import { describe, expect, test } from 'bun:test';
import type { SessionLogSnapshot } from '@/app/lib/diagnostics/sessionLog';
import type { AppReportInfo } from './reportInfo';
import {
  GITHUB_NEW_ISSUE_URL,
  MAX_ISSUE_URL_LENGTH,
  beforeReport,
  buildIssueUrl,
} from './issueUrl';

const T0 = 1_756_800_000_000;

const info: AppReportInfo = {
  appVersion: '0.3.2',
  os: 'macos 15.6 (aarch64)',
  webview: 'Mozilla/5.0 AppleWebKit/605.1.15',
  provider: 'r2',
  theme: 'dark',
  window: '1400x900 @2x',
  screen: '2560x1440',
  locale: 'en-US',
  capturedAt: '2026-09-02T10:00:00.000Z',
};

const snapshot = (msgs: string[]): SessionLogSnapshot => ({
  windowMinutes: 5,
  capturedTs: T0,
  truncated: false,
  entries: msgs.map((msg, i) => ({
    ts: T0 - (msgs.length - i) * 1000,
    kind: 'console',
    level: 'error',
    msg,
  })),
});

const bodyOf = (url: string): string => new URL(url).searchParams.get('body') ?? '';

describe('buildIssueUrl', () => {
  test('prefills title, label and a body with description, app info and the log', () => {
    const result = buildIssueUrl({
      title: 'Upload stalls',
      description: 'It stops at 99%.',
      info,
      log: snapshot(['Upload failed: timeout']),
    });
    const parsed = new URL(result.url);

    expect(result.url.startsWith(`${GITHUB_NEW_ISSUE_URL}?`)).toBe(true);
    expect(parsed.searchParams.get('labels')).toBe('bug');
    expect(parsed.searchParams.get('title')).toBe('Upload stalls');
    const body = bodyOf(result.url);
    expect(body).toContain('### What went wrong\n\nIt stops at 99%.');
    expect(body).toContain('| App version | 0.3.2 |');
    expect(body).toContain('| OS | macos 15.6 (aarch64) |');
    expect(body).toContain('| Window | 1400x900 @2x (screen 2560x1440) |');
    expect(body).toContain('1 line, 1 error, last 5 min');
    expect(body).toContain('-0:01 [error] Upload failed: timeout');
    expect(result.droppedLogLines).toBe(0);
    expect(result.descriptionTrimmed).toBe(false);
  });

  test('escapes table pipes and fills empty fields with unknown', () => {
    const result = buildIssueUrl({
      title: 't',
      description: 'd',
      info: { ...info, os: '', webview: 'a|b' },
      log: null,
    });
    const body = bodyOf(result.url);

    expect(body).toContain('| OS | unknown |');
    expect(body).toContain('| WebView | a\\|b |');
    expect(body).not.toContain('<details>');
  });

  test('falls back to a default title', () => {
    const result = buildIssueUrl({ title: '  ', description: 'd', info, log: null });

    expect(new URL(result.url).searchParams.get('title')).toBe('Problem report');
  });

  test('drops the oldest log lines first when the link would be too long', () => {
    const msgs = Array.from({ length: 80 }, (_, i) => `line ${i} ${'x'.repeat(150)}`);
    const result = buildIssueUrl({
      title: 'Long log',
      description: 'short',
      info,
      log: snapshot(msgs),
    });

    expect(result.url.length).toBeLessThanOrEqual(MAX_ISSUE_URL_LENGTH);
    expect(result.droppedLogLines).toBeGreaterThan(0);
    expect(result.droppedLogLines).toBeLessThan(msgs.length);
    expect(result.descriptionTrimmed).toBe(false);
    const body = bodyOf(result.url);
    expect(body).toContain('line 79 ');
    expect(body).not.toContain('line 0 ');
    expect(body).toContain(`${result.droppedLogLines} older lines were left out`);
  });

  test('cuts the description only when it cannot fit even without the log', () => {
    const result = buildIssueUrl({
      title: 'Huge',
      description: 'word '.repeat(3000),
      info,
      log: snapshot(['e1', 'e2']),
    });

    expect(result.url.length).toBeLessThanOrEqual(MAX_ISSUE_URL_LENGTH);
    expect(result.descriptionTrimmed).toBe(true);
    expect(result.droppedLogLines).toBe(2);
    const body = bodyOf(result.url);
    expect(body).toContain('Description cut to fit the link');
    expect(body).toContain('| App version | 0.3.2 |');
  });

  test('bounds oversized app-info cells so the fixed part of the link always fits', () => {
    const result = buildIssueUrl({
      title: 'x'.repeat(120),
      description: 'd',
      info: { ...info, webview: 'W'.repeat(5000), os: 'O'.repeat(5000) },
      log: snapshot([]),
    });

    expect(result.url.length).toBeLessThanOrEqual(MAX_ISSUE_URL_LENGTH);
    expect(result.descriptionTrimmed).toBe(false);
    const body = bodyOf(result.url);
    expect(body).toContain(`${'W'.repeat(391)}…`);
    expect(body).not.toContain('W'.repeat(392));
  });

  test('bounds app-info cells by encoded length, so non-ascii values cannot blow the budget', () => {
    const cjk = '漢'.repeat(300);
    const result = buildIssueUrl({
      title: '標'.repeat(120),
      description: 'd',
      info: {
        appVersion: cjk,
        os: cjk,
        webview: cjk,
        provider: cjk,
        theme: cjk,
        window: cjk,
        screen: cjk,
        locale: cjk,
        capturedAt: cjk,
      },
      log: snapshot([]),
    });

    expect(result.url.length).toBeLessThanOrEqual(MAX_ISSUE_URL_LENGTH);
    expect(result.descriptionTrimmed).toBe(false);
    expect(bodyOf(result.url)).toContain('漢…');
  });

  test('never splits a surrogate pair when cutting', () => {
    const result = buildIssueUrl({
      title: 'Emoji',
      description: '😀'.repeat(2000),
      info,
      log: null,
    });

    expect(result.descriptionTrimmed).toBe(true);
    const body = bodyOf(result.url);
    expect(body).not.toMatch(/[\uD800-\uDBFF](?![\uDC00-\uDFFF])/);
    expect(body).toContain('😀');
  });
});

describe('beforeReport', () => {
  test('formats the countdown to the report click', () => {
    expect(beforeReport(T0 - 8_000, T0)).toBe('-0:08');
    expect(beforeReport(T0 - 252_000, T0)).toBe('-4:12');
    expect(beforeReport(T0 + 5_000, T0)).toBe('-0:00');
  });
});
