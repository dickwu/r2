'use client';

import { useEffect, useState, useCallback, useRef } from 'react';
import { getVersion } from '@tauri-apps/api/app';
import { check, type Update } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';
import { Button, Badge, Tooltip, App } from 'antd';
import { CloudSyncOutlined } from '@ant-design/icons';

const AUTO_CHECK_DELAY_MS = 3_000;
const AUTO_CHECK_INTERVAL_MS = 30 * 60 * 1_000;

export default function UpdateChecker() {
  const [appVersion, setAppVersion] = useState<string>('');
  const [updateAvailable, setUpdateAvailable] = useState<{
    version: string;
    body?: string;
  } | null>(null);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const { message, modal } = App.useApp();
  const modalShownForRef = useRef<string | null>(null);

  const promptUpdate = useCallback(
    (update: Update) => {
      if (modalShownForRef.current === update.version) return;
      modalShownForRef.current = update.version;

      setUpdateAvailable({ version: update.version, body: update.body ?? undefined });

      modal.confirm({
        title: `Update Available: v${update.version}`,
        content: update.body || 'A new version is available. Would you like to update now?',
        okText: 'Update & Restart',
        cancelText: 'Later',
        onOk: async () => {
          message.loading({ content: 'Downloading update...', key: 'update', duration: 0 });
          await update.downloadAndInstall();
          message.success({ content: 'Update installed! Restarting...', key: 'update' });
          await relaunch();
        },
        onCancel: () => {
          modalShownForRef.current = null;
        },
      });
    },
    [message, modal]
  );

  const checkForUpdate = useCallback(
    async (opts?: { silent?: boolean }) => {
      const silent = opts?.silent ?? false;
      setCheckingUpdate(true);
      try {
        const update = await check();
        if (update) {
          promptUpdate(update);
        } else {
          setUpdateAvailable(null);
          if (!silent) {
            message.success("You're on the latest version!");
          }
        }
      } catch (e) {
        if (!silent) {
          message.error('Failed to check for updates');
        }
      } finally {
        setCheckingUpdate(false);
      }
    },
    [message, promptUpdate]
  );

  useEffect(() => {
    async function init() {
      try {
        const version = await getVersion();
        setAppVersion(version);
      } catch {
        // non-Tauri env
      }
    }
    init();

    const startupTimer = setTimeout(() => {
      checkForUpdate({ silent: true });
    }, AUTO_CHECK_DELAY_MS);

    const interval = setInterval(() => {
      checkForUpdate({ silent: true });
    }, AUTO_CHECK_INTERVAL_MS);

    return () => {
      clearTimeout(startupTimer);
      clearInterval(interval);
    };
  }, [checkForUpdate]);

  return (
    <Tooltip title="Check for updates">
      <Badge dot={!!updateAvailable} offset={[-2, 2]}>
        <Button
          type="text"
          size="small"
          icon={<CloudSyncOutlined spin={checkingUpdate} />}
          onClick={() => checkForUpdate()}
          loading={checkingUpdate}
        >
          v{appVersion || '...'}
        </Button>
      </Badge>
    </Tooltip>
  );
}
