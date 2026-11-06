import { t as i18nT } from '@/i18n';
/**
 * Updater errors arrive as raw plugin strings ("error sending request for url
 * (…)", "signature verification failed", "Permission denied (os error 13)").
 * This turns them into what happened and what to do, in the app's words. The
 * raw text stays available as `detail` for the dialog's details and for bug
 * reports.
 */

export type UpdateStep = 'check' | 'download' | 'install' | 'restart';

export type UpdateErrorKind =
  'network' | 'signature' | 'permission' | 'disk' | 'server' | 'unknown';

export interface UpdateFailure {
  step: UpdateStep;
  kind: UpdateErrorKind;
  /** One or two sentences: what happened, then how to fix it. */
  message: string;
  /** The plugin's own text. */
  detail: string;
  /** Whether Try again is likely to help (Download from GitHub is always offered). */
  retryable: boolean;
}

export const errorText = (error: unknown): string => {
  if (error instanceof Error) return error.message;
  if (typeof error === 'string') return error;
  try {
    return JSON.stringify(error) ?? String(error);
  } catch {
    return String(error);
  }
};

const PATTERNS: Array<{ kind: UpdateErrorKind; re: RegExp }> = [
  { kind: 'signature', re: /signature|minisign|pubkey|public key|untrusted/i },
  {
    kind: 'disk',
    re: /no space|disk (is )?full|ENOSPC|not enough space|insufficient (disk )?space|os error 28|os error 112/i,
  },
  {
    kind: 'permission',
    re: /permission denied|access is denied|operation not permitted|EACCES|EPERM|read-only file system|os error (1|5|13)\b|requires elevation|administrator/i,
  },
  {
    kind: 'server',
    re: /could not fetch a valid release json|status code[: ]*(4\d\d|5\d\d)|\b(404|500|502|503|504)\b|invalid (release|json)|failed to deserialize|expected value/i,
  },
  {
    kind: 'network',
    re: /network|error sending request|dns|resolve|timed? ?out|timeout|connect(ion)?|offline|unreachable|tls|certificate|socket|reset by peer|broken pipe|decoding response body|unexpected eof/i,
  },
];

export const classifyUpdateError = (detail: string): UpdateErrorKind =>
  PATTERNS.find(({ re }) => re.test(detail))?.kind ?? 'unknown';

const stepFailed = (step: UpdateStep): string => i18nT(`updates.error.step.${step}`);

const messageFor = (step: UpdateStep, kind: UpdateErrorKind): string => {
  if (step === 'restart') return i18nT('updates.error.restart', { step: stepFailed('restart') });
  switch (kind) {
    case 'network':
      return step === 'check'
        ? i18nT('updates.error.networkCheck')
        : i18nT('updates.error.network', { step: stepFailed(step) });
    case 'signature':
      return i18nT('updates.error.signature');
    case 'permission':
      return i18nT('updates.error.permission');
    case 'disk':
      return i18nT('updates.error.disk');
    case 'server':
      return step === 'check'
        ? i18nT('updates.error.serverCheck')
        : i18nT('updates.error.server', { step: stepFailed(step) });
    default:
      return i18nT('updates.error.unknown', { step: stepFailed(step) });
  }
};

export const describeUpdateError = (step: UpdateStep, error: unknown): UpdateFailure => {
  const detail = errorText(error).trim() || i18nT('updates.error.noDetails');
  const kind = classifyUpdateError(detail);
  return {
    step,
    kind,
    message: messageFor(step, kind),
    detail,
    retryable: step !== 'restart' && kind !== 'signature' && kind !== 'permission',
  };
};

/** The few words the status bar has room for. */
export const shortFailure = (failure: UpdateFailure): string =>
  failure.step === 'check' ? i18nT('updates.error.shortCheck') : i18nT('updates.error.shortFailed');
