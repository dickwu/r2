import { redactSecrets } from './redact';
import { logSession } from './sessionLog';

/**
 * Wires the app's own complaints into the session log: the warnings and errors
 * it already prints to a devtools console nobody has open, plus the two
 * failure channels that print nothing at all — an uncaught error and a
 * rejected promise.
 *
 * Deliberately not `console.log`/`info`/`debug`: those are the chatty ones, and
 * chat is what pushes the five-minute window out of the buffer. Warnings and
 * errors are the lines someone reading an issue actually wants.
 */

/** Longest a single console argument may contribute. */
const MAX_ARGUMENT_LENGTH = 200;

let installed = false;

/** One console argument as a line of text — never throws, whatever it is. */
const describe = (value: unknown): string => {
  if (typeof value === 'string') return value;
  if (value instanceof Error) return `${value.name}: ${value.message}`;
  if (value === null) return 'null';
  if (value === undefined) return 'undefined';

  try {
    if (typeof value === 'object') {
      // Redact before cutting: a cut could otherwise leave a fragment of a
      // secret too short for any rule to recognise, and logSession's own
      // redaction would then run over the fragment, not the secret.
      return redactSecrets(JSON.stringify(value)).slice(0, MAX_ARGUMENT_LENGTH);
    }
    return String(value);
  } catch {
    return '[unprintable]';
  }
};

/**
 * Wrapping console means every `console.error` in the app now runs this first.
 * If it could throw, the logger would turn an error someone was merely
 * reporting into an error that breaks the caller — so the join is guarded,
 * not just the parts known to be risky.
 */
export const describeConsoleArgs = (args: readonly unknown[]): string => {
  try {
    return args.map(describe).join(' ');
  } catch {
    return '[unprintable]';
  }
};

/**
 * Start recording. Idempotent — the app shell mounts once, but a fast-refresh
 * cycle in dev would otherwise stack wrappers until every error logged twice.
 */
export const installSessionLog = (): void => {
  if (installed || typeof window === 'undefined') return;
  installed = true;

  const nativeWarn = console.warn;
  const nativeError = console.error;

  console.warn = (...args: unknown[]) => {
    logSession('console', 'warn', describeConsoleArgs(args));
    nativeWarn.apply(console, args);
  };

  console.error = (...args: unknown[]) => {
    logSession('console', 'error', describeConsoleArgs(args));
    nativeError.apply(console, args);
  };

  window.addEventListener('error', (event) => {
    // Resource errors (a failed <img>) also land here and carry no message.
    const where = event.filename ? ` (${event.filename}:${event.lineno})` : '';
    logSession('console', 'error', `Uncaught ${event.message || 'error'}${where}`);
  });

  window.addEventListener('unhandledrejection', (event) => {
    logSession('console', 'error', `Unhandled rejection: ${describe(event.reason)}`);
  });

  logSession('app', 'info', 'Session started');
};
