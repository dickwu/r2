import { redactSecrets } from './redact';

/**
 * The last few minutes of what the app was doing, kept in memory so a problem
 * report can carry the moments that led up to it.
 *
 * A report says what the person saw; this says what the app did. Three rules
 * keep it safe to paste into a public issue: every line is redacted and
 * truncated as it is written, the buffer is bounded, and the reporter sees the
 * exact lines before anything leaves the app.
 */

export type SessionLogKind = 'console' | 'app';

export type SessionLogLevel = 'info' | 'warn' | 'error';

export interface SessionLogEntry {
  /** Epoch ms — what the window and the T-minus gutter are computed from. */
  ts: number;
  kind: SessionLogKind;
  level: SessionLogLevel;
  msg: string;
}

/** The tail as it rides inside a report. */
export interface SessionLogSnapshot {
  windowMinutes: number;
  /**
   * When the snapshot was taken, epoch ms. Every entry is read as a countdown
   * to this: "the error came eight seconds before they hit Report" is the
   * finding, not the wall clock.
   */
  capturedTs: number;
  /**
   * The window is missing its start: the app was busier than the buffer, so
   * the oldest lines fell out. Worth knowing before concluding "nothing
   * happened before the error".
   */
  truncated: boolean;
  entries: SessionLogEntry[];
}

/** How far back a report reaches. */
export const SESSION_LOG_WINDOW_MINUTES = 5;

const WINDOW_MS = SESSION_LOG_WINDOW_MINUTES * 60 * 1000;

/**
 * Ring size. Five minutes of ordinary use is well under this; a render loop
 * hammering a failing call blows past it, which is exactly the case
 * `truncated` exists to declare.
 */
const MAX_ENTRIES = 200;

/** Per-line cap. Long enough for a stack's first frame, short enough to scan. */
const MAX_MESSAGE_LENGTH = 300;

let entries: readonly SessionLogEntry[] = [];

/** When the most recently evicted line happened; -Infinity until the ring first overflows. */
let lastEvictedTs = Number.NEGATIVE_INFINITY;

const truncate = (value: string): string =>
  value.length > MAX_MESSAGE_LENGTH ? `${value.slice(0, MAX_MESSAGE_LENGTH)}…` : value;

/**
 * Record one moment. Never throws and never awaits: this sits under every
 * console.error in the app, so a broken logger must not be able to break the
 * code it is watching. Redaction runs before truncation so a cut line can
 * never end in half a secret.
 */
export const logSession = (
  kind: SessionLogKind,
  level: SessionLogLevel,
  msg: string,
  now: number = Date.now()
): void => {
  try {
    const text = truncate(redactSecrets(msg).replace(/\s+/g, ' ').trim());
    if (text === '') return;

    const entry: SessionLogEntry = { ts: now, kind, level, msg: text };
    if (entries.length >= MAX_ENTRIES) {
      lastEvictedTs = entries[0].ts;
      entries = [...entries.slice(entries.length - MAX_ENTRIES + 1), entry];
    } else {
      entries = [...entries, entry];
    }
  } catch {
    // A logger that can fail the caller is worse than no logger.
  }
};

/**
 * The tail as a report should carry it: everything inside the window, oldest
 * first. Reads are pure — taking a snapshot does not consume the buffer, so
 * cancelling a report and filing another still gets the full five minutes.
 */
export const recentSessionLog = (now: number = Date.now()): SessionLogSnapshot => {
  const cutoff = now - WINDOW_MS;
  const within = entries.filter((entry) => entry.ts >= cutoff);

  return {
    windowMinutes: SESSION_LOG_WINDOW_MINUTES,
    capturedTs: now,
    // Only an eviction that fell inside this window counts: a ring that merely
    // filled up, or that overflowed ten minutes ago, has dropped nothing here.
    truncated: lastEvictedTs >= cutoff,
    entries: within,
  };
};

/** Drop the tail. Tests use this; the app keeps one buffer per process. */
export const clearSessionLog = (): void => {
  entries = [];
  lastEvictedTs = Number.NEGATIVE_INFINITY;
};
