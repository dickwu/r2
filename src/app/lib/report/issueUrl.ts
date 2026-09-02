import type { SessionLogEntry, SessionLogSnapshot } from '@/app/lib/diagnostics/sessionLog';
import type { AppReportInfo } from './reportInfo';

export const GITHUB_REPO_URL = 'https://github.com/dickwu/r2';
export const GITHUB_NEW_ISSUE_URL = `${GITHUB_REPO_URL}/issues/new`;

/**
 * GitHub stops honouring the prefill once the new-issue URL grows past roughly
 * 8 KB, so the whole link is kept under this. When something has to go, the
 * log goes first (oldest lines first) and the description last — what the
 * reporter typed outranks what the app recorded.
 */
export const MAX_ISSUE_URL_LENGTH = 7000;

const DEFAULT_TITLE = 'Problem report';
const TRIMMED_NOTICE = '\n\n_(Description cut to fit the link.)_';

export interface IssueDraft {
  title: string;
  description: string;
  info: AppReportInfo;
  /** null = the reporter chose not to include the log. */
  log: SessionLogSnapshot | null;
}

export interface IssueUrlResult {
  url: string;
  /** Log lines left out because the link would have been too long. */
  droppedLogLines: number;
  /** The description had to be cut to fit the link. */
  descriptionTrimmed: boolean;
}

/** Countdown to the report click, e.g. '-4:12'. */
export const beforeReport = (ts: number, capturedTs: number): string => {
  const seconds = Math.max(0, Math.round((capturedTs - ts) / 1000));
  const minutes = Math.floor(seconds / 60);
  return `-${minutes}:${String(seconds % 60).padStart(2, '0')}`;
};

export const formatLogLine = (entry: SessionLogEntry, capturedTs: number): string =>
  `${beforeReport(entry.ts, capturedTs)} [${entry.level}] ${entry.msg}`;

const plural = (count: number, noun: string): string => `${count} ${noun}${count === 1 ? '' : 's'}`;

/**
 * Longest a single table cell may be once URL-encoded. Measured encoded, like
 * the budget itself, so a cell of CJK text (nine encoded characters each) is
 * bounded exactly as tightly as ASCII — the fixed part of the body then always
 * fits, and only the description and the log ever need cutting.
 */
const MAX_CELL_ENCODED_LENGTH = 400;

/** The longest prefix of `text` (by code point) whose encoded form fits, marked with an ellipsis. */
const clampEncoded = (text: string, maxEncoded: number): string => {
  if (encodeURIComponent(text).length <= maxEncoded) return text;
  const chars = Array.from(text);
  const ellipsisLength = encodeURIComponent('…').length;
  const kept = largestFitting(
    chars.length,
    (n) => encodeURIComponent(chars.slice(0, n).join('')).length + ellipsisLength <= maxEncoded
  );
  return `${chars.slice(0, kept).join('')}…`;
};

/** One markdown table cell: never empty, never containing a bare pipe, never huge. */
const cell = (value: string): string => {
  const text = value.replace(/\s+/g, ' ').replace(/\|/g, '\\|').trim();
  return text === '' ? 'unknown' : clampEncoded(text, MAX_CELL_ENCODED_LENGTH);
};

const infoTable = (info: AppReportInfo): string => {
  const rows: Array<[string, string]> = [
    ['App version', info.appVersion],
    ['OS', info.os],
    ['WebView', info.webview],
    ['Provider', info.provider],
    ['Theme', info.theme],
    ['Window', info.screen ? `${info.window} (screen ${info.screen})` : info.window],
    ['Locale', info.locale],
    ['Reported at', info.capturedAt],
  ];
  return [
    '| Field | Value |',
    '| --- | --- |',
    ...rows.map(([field, value]) => `| ${field} | ${cell(value)} |`),
  ].join('\n');
};

interface LogSectionInput {
  log: SessionLogSnapshot;
  /** The lines that made the cut, oldest first. */
  lines: string[];
  dropped: number;
}

const logSection = ({ log, lines, dropped }: LogSectionInput): string => {
  const errors = log.entries.filter((entry) => entry.level === 'error').length;
  const summary = `Recent app log — ${plural(log.entries.length, 'line')}, ${plural(errors, 'error')}, last ${log.windowMinutes} min`;
  const notes = [
    log.truncated ? '_The app was busy; the oldest lines of this window were discarded._' : '',
    dropped > 0 ? `_${dropped} older lines were left out to keep the link short._` : '',
    log.entries.length === 0 ? '_No warnings or errors were recorded._' : '',
  ].filter(Boolean);
  // A four-backtick fence survives a logged line that itself contains ```.
  const block = lines.length > 0 ? ['````', ...lines, '````'].join('\n') : '';

  return ['<details>', `<summary>${summary}</summary>`, '', ...notes, block, '</details>']
    .filter((part) => part !== '')
    .join('\n');
};

interface BodyParts {
  description: string;
  info: AppReportInfo;
  log: SessionLogSnapshot | null;
  lines: string[];
  dropped: number;
  trimmed: boolean;
}

export const buildIssueBody = ({
  description,
  info,
  log,
  lines,
  dropped,
  trimmed,
}: BodyParts): string =>
  [
    '### What went wrong',
    '',
    `${description}${trimmed ? TRIMMED_NOTICE : ''}`,
    '',
    '### App info',
    '',
    infoTable(info),
    ...(log ? ['', logSection({ log, lines, dropped })] : []),
    '',
    '<sub>Filed from the R2 Client "Report a problem" button.</sub>',
  ].join('\n');

const issueUrl = (title: string, body: string): string =>
  `${GITHUB_NEW_ISSUE_URL}?labels=bug&title=${encodeURIComponent(title)}&body=${encodeURIComponent(body)}`;

/** Largest n in [0, max] for which `fits(n)` holds; `fits` must be monotonic. */
const largestFitting = (max: number, fits: (n: number) => boolean): number => {
  let low = 0;
  let high = max;
  while (low < high) {
    const mid = Math.ceil((low + high) / 2);
    if (fits(mid)) low = mid;
    else high = mid - 1;
  }
  return low;
};

/**
 * The prefilled new-issue link for a draft, guaranteed to fit GitHub's URL
 * budget. Log lines are dropped oldest-first before a single character of the
 * description is touched; the description is cut by code point so a surrogate
 * pair is never split into something `encodeURIComponent` rejects.
 */
export const buildIssueUrl = (draft: IssueDraft): IssueUrlResult => {
  const title = draft.title.trim() || DEFAULT_TITLE;
  const description = draft.description.trim();
  const { info, log } = draft;
  const allLines = log ? log.entries.map((entry) => formatLogLine(entry, log.capturedTs)) : [];

  const fits = (url: string): boolean => url.length <= MAX_ISSUE_URL_LENGTH;
  const urlWith = (keptLines: number, text: string, trimmed: boolean): string =>
    issueUrl(
      title,
      buildIssueBody({
        description: text,
        info,
        log,
        lines: allLines.slice(allLines.length - keptLines),
        dropped: allLines.length - keptLines,
        trimmed,
      })
    );

  const full = urlWith(allLines.length, description, false);
  if (fits(full)) {
    return { url: full, droppedLogLines: 0, descriptionTrimmed: false };
  }

  if (fits(urlWith(0, description, false))) {
    const kept = largestFitting(allLines.length, (n) => fits(urlWith(n, description, false)));
    return {
      url: urlWith(kept, description, false),
      droppedLogLines: allLines.length - kept,
      descriptionTrimmed: false,
    };
  }

  const chars = Array.from(description);
  const kept = largestFitting(chars.length, (n) =>
    fits(urlWith(0, chars.slice(0, n).join(''), true))
  );
  return {
    url: urlWith(0, chars.slice(0, kept).join(''), true),
    droppedLogLines: allLines.length,
    descriptionTrimmed: true,
  };
};
