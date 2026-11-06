/**
 * Pure helpers for the bucket mount UI: host-OS detection and the per-OS copy
 * and path rules that follow from it. Kept free of React/Tauri imports so the
 * behaviour is unit-testable.
 */

import { t as i18nT } from '@/i18n';

export type OsKind = 'macos' | 'windows' | 'linux';

/**
 * The webview reports the host OS in its user agent — WKWebView on macOS,
 * WebView2 on Windows, WebKitGTK on Linux.
 */
export function detectOs(userAgent?: string): OsKind {
  const ua = userAgent ?? (typeof navigator === 'undefined' ? '' : navigator.userAgent);
  if (/Windows|Win64|Win32/i.test(ua)) return 'windows';
  if (/Macintosh|Mac OS X/i.test(ua)) return 'macos';
  return 'linux';
}

/** What the OS calls its file manager, for buttons that open one. */
export function revealActionLabel(os: OsKind): string {
  if (os === 'windows') return i18nT('mount.reveal.windows');
  if (os === 'linux') return i18nT('mount.reveal.linux');
  return i18nT('mount.reveal.macos');
}

/** One line on what mounting needs from this OS, shown before the user commits. */
export function mountRequirementHint(os: OsKind): string {
  if (os === 'windows') return i18nT('mount.requirement.windows');
  if (os === 'linux') return i18nT('mount.requirement.linux');
  return i18nT('mount.requirement.macos');
}

export function pathSeparator(os: OsKind): string {
  return os === 'windows' ? '\\' : '/';
}

/**
 * Build a mount point inside a folder the user picked. The bucket becomes the
 * leaf folder — mount points must be their own empty directory — unless the
 * pick already ends with the bucket name.
 */
export function joinMountPath(parent: string, bucket: string, os: OsKind): string {
  const trimmed = parent.replace(/[/\\]+$/, '');
  const leaf = trimmed.split(/[/\\]/).pop();
  if (leaf === bucket) return trimmed;
  return `${trimmed}${pathSeparator(os)}${bucket}`;
}

/** The last path segment, for labelling the folder end of the mount schematic. */
export function pathLeaf(path: string): string {
  const trimmed = path.replace(/[/\\]+$/, '');
  const leaf = trimmed.split(/[/\\]/).pop();
  return leaf || path;
}

/**
 * Shorten from the middle, so a long mount path keeps both the volume it
 * starts from and the folder it ends at — the two parts that identify it.
 */
export function middleTruncate(text: string, max = 46): string {
  if (max < 1) return '';
  if (text.length <= max) return text;
  if (max === 1) return '…';
  const keep = max - 1;
  const head = Math.ceil(keep / 2);
  const tail = keep - head;
  return `${text.slice(0, head)}…${tail > 0 ? text.slice(text.length - tail) : ''}`;
}

/**
 * How long a mount has been up, in the same vocabulary the status bar uses.
 * `mountedAt` is the backend's unix timestamp, in seconds.
 */
export function relativeMountTime(mountedAt: number, nowMs: number = Date.now()): string {
  const diffMin = Math.floor((nowMs - mountedAt * 1000) / 60_000);
  if (diffMin < 1) return i18nT('time.justNow');
  if (diffMin < 60) return i18nT('time.minAgo', { count: diffMin });
  const diffHr = Math.floor(diffMin / 60);
  if (diffHr < 24) return i18nT('time.hrAgo', { count: diffHr });
  const diffDay = Math.floor(diffHr / 24);
  return i18nT('time.dayAgo', { count: diffDay });
}

/** What a mount lets you do, named the same way wherever the mode is shown. */
export function mountModeLabel(readOnly: boolean): string {
  return readOnly ? i18nT('mount.mode.readOnly') : i18nT('mount.mode.writable');
}

/** The consequence of that mode, in one line. */
export function mountModeHint(readOnly: boolean): string {
  return readOnly ? i18nT('mount.mode.readOnlyHint') : i18nT('mount.mode.writableHint');
}

/**
 * A Linux mount that needs elevation comes back with the exact command to run.
 * Pull that line out so it can be shown as copyable code.
 */
export function extractSudoCommand(error: string | null | undefined): string | null {
  if (!error) return null;
  for (const raw of error.split('\n')) {
    const line = raw.trim();
    if (line.startsWith('sudo mount')) return line;
  }
  return null;
}

/** The rest of the error, once the command has been lifted out into its own block. */
export function messageWithoutSudoCommand(error: string | null | undefined): string {
  if (!error) return '';
  return error
    .split('\n')
    .filter((raw) => !raw.trim().startsWith('sudo mount'))
    .join('\n')
    .trim();
}
