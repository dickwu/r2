import { formatBytes } from '@/app/utils/formatBytes';
import { t as i18nT } from '@/i18n';
import type { DownloadProgress, UpdateInfo, UpdatePhase } from '@/app/stores/updateStore';
import { shortFailure, type UpdateFailure } from './updateErrors';

/**
 * What the status bar's update control and Settings' update link say for each
 * updater state, in one place so both read the same. Pure, so the tests can
 * walk every state.
 */

export type StatusTone = 'default' | 'accent' | 'error' | 'muted';
export type StatusAction = 'check' | 'openDialog' | 'retryCheck' | 'none';

export interface UpdateStatusView {
  label: string;
  /** The tooltip: the version you have, or what clicking does. */
  hint: string;
  tone: StatusTone;
  action: StatusAction;
  /** Only for a check you asked for: background checks stay still. */
  spinning: boolean;
}

export interface UpdateStatusInput {
  phase: UpdatePhase;
  manualCheck: boolean;
  currentVersion: string | null;
  update: UpdateInfo | null;
  progress: DownloadProgress;
  failure: UpdateFailure | null;
}

export const progressPercent = (progress: DownloadProgress): number | null =>
  progress.total ? Math.min(100, Math.floor((progress.downloaded / progress.total) * 100)) : null;

/** "4.2 MB of 9.8 MB", or "4.2 MB" without a size. */
export const progressAmount = (progress: DownloadProgress): string =>
  progress.total
    ? i18nT('transfer.amountOf', {
        done: formatBytes(progress.downloaded),
        total: formatBytes(progress.total),
      })
    : formatBytes(progress.downloaded);

export const updateStatusView = ({
  phase,
  manualCheck,
  currentVersion,
  update,
  progress,
  failure,
}: UpdateStatusInput): UpdateStatusView => {
  const have = currentVersion ? `v${currentVersion}` : 'R2 Client';
  const offered = update ? `v${update.version}` : '';

  switch (phase) {
    case 'checking':
      if (manualCheck) {
        return {
          label: i18nT('updates.status.checking'),
          hint: have,
          tone: 'muted',
          action: 'none',
          spinning: true,
        };
      }
      break;
    case 'upToDate':
      return {
        label: i18nT('updates.status.upToDate'),
        hint: i18nT('updates.status.upToDateHint', { version: have }),
        tone: 'default',
        action: 'check',
        spinning: false,
      };
    case 'downloading': {
      const percent = progressPercent(progress);
      return {
        label:
          percent === null
            ? i18nT('updates.status.downloading')
            : i18nT('updates.status.downloadingPercent', { percent }),
        hint: i18nT('updates.status.downloadingHint', { version: offered }),
        tone: 'accent',
        action: 'openDialog',
        spinning: false,
      };
    }
    case 'installing':
      return {
        label: i18nT('updates.status.installing'),
        hint: offered,
        tone: 'accent',
        action: 'openDialog',
        spinning: false,
      };
    case 'readyToRestart':
      return {
        label: i18nT('updates.status.restartToUpdate'),
        hint: i18nT('updates.status.restartHint', { version: offered }),
        tone: 'accent',
        action: 'openDialog',
        spinning: false,
      };
    case 'restarting':
      return {
        label: i18nT('updates.status.restarting'),
        hint: offered,
        tone: 'muted',
        action: 'none',
        spinning: false,
      };
    case 'error':
      if (failure) {
        return {
          label: shortFailure(failure),
          hint: failure.message,
          tone: 'error',
          action: failure.step === 'check' ? 'retryCheck' : 'openDialog',
          spinning: false,
        };
      }
      break;
  }

  if (update) {
    return {
      label: i18nT('updates.status.available', { version: offered }),
      hint: i18nT('updates.status.availableHint', { version: have }),
      tone: 'accent',
      action: 'openDialog',
      spinning: false,
    };
  }
  return {
    label: currentVersion ? `v${currentVersion}` : 'v…',
    hint: i18nT('settings.checkForUpdates'),
    tone: 'default',
    action: 'check',
    spinning: false,
  };
};
