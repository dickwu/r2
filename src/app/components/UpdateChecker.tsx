'use client';

import { Button, Tooltip } from 'antd';
import {
  CheckOutlined,
  CloudDownloadOutlined,
  LoadingOutlined,
  WarningOutlined,
} from '@ant-design/icons';
import { useAppUpdater } from '@/app/hooks/useAppUpdater';
import { updateStatusView } from '@/app/lib/updates/updateStatus';
import UpdateModal from '@/app/components/updates/UpdateModal';
import ReleaseNotesModal from '@/app/components/updates/ReleaseNotesModal';
import WhatsNewNotice from '@/app/components/updates/WhatsNewNotice';

const TONE_CLASS = {
  default: 'sb-update',
  muted: 'sb-update sb-update-muted',
  accent: 'sb-update sb-update-accent',
  error: 'sb-update sb-update-error',
} as const;

/**
 * The status bar's version, which turns into the update's state: a check you
 * asked for and its result, "v0.3.18 available", download progress, and
 * "Restart to update". It also hosts the update dialog, What's new, and the
 * release notes, so the updater runs once for the whole app.
 */
export default function UpdateChecker() {
  const updater = useAppUpdater();
  const view = updateStatusView(updater);

  const onClick = () => {
    if (view.action === 'check') void updater.checkForUpdates({ manual: true });
    else if (view.action === 'retryCheck') void updater.retry();
    else if (view.action === 'openDialog') updater.openDialog();
  };

  const icon = view.spinning ? (
    <LoadingOutlined />
  ) : updater.phase === 'upToDate' ? (
    <CheckOutlined />
  ) : view.tone === 'error' ? (
    <WarningOutlined />
  ) : view.tone === 'accent' ? (
    <CloudDownloadOutlined />
  ) : undefined;

  return (
    <>
      <Tooltip title={view.hint}>
        <Button
          type="text"
          size="small"
          className={TONE_CLASS[view.tone]}
          icon={icon}
          onClick={onClick}
          aria-disabled={view.action === 'none'}
        >
          <span aria-live="polite">{view.label}</span>
        </Button>
      </Tooltip>
      <UpdateModal />
      <ReleaseNotesModal />
      <WhatsNewNotice />
    </>
  );
}
