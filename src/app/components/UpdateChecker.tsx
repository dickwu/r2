'use client';

import { useEffect, useState, useCallback } from 'react';
import { getVersion } from '@tauri-apps/api/app';
import { check } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';
import { Button, Badge, Tooltip, App } from 'antd';
import { CloudSyncOutlined } from '@ant-design/icons';

export default function UpdateChecker() {
  const [appVersion, setAppVersion] = useState<string>('');
  const [updateAvailable, setUpdateAvailable] = useState<{ version: string; body?: string } | null>(
    null
  );
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const { message, modal } = App.useApp();

  useEffect(() => {
    loadVersion();
  }, []);

  async function loadVersion() {
    try {
      const version = await getVersion();
      setAppVersion(version);
    } catch (e) {
      console.error('Failed to get version:', e);
    }
  }

  const checkForUpdate = useCallback(async () => {
    setCheckingUpdate(true);
    try {
      const update = await check();
      if (update) {
        setUpdateAvailable({ version: update.version, body: update.body ?? undefined });
        modal.confirm({
          title: `Update Available: v${update.version}`,
          content: update.body || 'A new version is available. Would you like to update now?',
          okText: 'Update Now',
          cancelText: 'Later',
          onOk: async () => {
            message.loading({ content: 'Downloading update...', key: 'update', duration: 0 });
            await update.downloadAndInstall();
            message.success({ content: 'Update installed! Restarting...', key: 'update' });
            await relaunch();
          },
        });
      } else {
        message.success("You're on the latest version!");
        setUpdateAvailable(null);
      }
    } catch (e) {
      console.error('Update check failed:', e);
      message.error('Failed to check for updates');
    } finally {
      setCheckingUpdate(false);
    }
  }, [message, modal]);

  return (
    <Tooltip title="Check for updates">
      <Badge dot={!!updateAvailable} offset={[-2, 2]}>
        <Button
          type="text"
          size="small"
          icon={<CloudSyncOutlined spin={checkingUpdate} />}
          onClick={checkForUpdate}
          loading={checkingUpdate}
        >
          v{appVersion || '...'}
        </Button>
      </Badge>
    </Tooltip>
  );
}
