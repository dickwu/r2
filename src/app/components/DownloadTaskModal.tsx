'use client';

import { useTranslation } from 'react-i18next';
import { useEffect, useState, useMemo, useCallback } from 'react';
import { Modal, Button, Empty, App, Tabs, Badge, Popconfirm } from 'antd';
import { invoke } from '@tauri-apps/api/core';
import { useAntdModalFocusable } from '@/app/hooks/useAntdModalFocusable';
import { Virtuoso } from 'react-virtuoso';
import {
  useDownloadStore,
  selectPendingCount,
  selectDownloadingCount,
  selectPausedCount,
  selectFinishedCount,
  selectHasActiveDownloads,
  DownloadSession,
  DownloadTask,
} from '@/app/stores/downloadStore';
import DownloadTaskItem from '@/app/components/DownloadTaskItem';
import { formatBytes, formatEta } from '@/app/utils/formatBytes';
import type { StorageConfig } from '@/app/lib/r2cache';

interface DownloadTaskModalProps {
  storageConfig?: StorageConfig | null;
}

export default function DownloadTaskModal({ storageConfig }: DownloadTaskModalProps) {
  const tasks = useDownloadStore((state) => state.tasks);
  const modalOpen = useDownloadStore((state) => state.modalOpen);
  const setModalOpen = useDownloadStore((state) => state.setModalOpen);
  const loadFromDatabase = useDownloadStore((state) => state.loadFromDatabase);
  const focusable = useAntdModalFocusable();

  const pendingCount = useDownloadStore(selectPendingCount);
  const downloadingCount = useDownloadStore(selectDownloadingCount);
  const pausedCount = useDownloadStore(selectPausedCount);
  const finishedCount = useDownloadStore(selectFinishedCount);
  const hasActiveDownloads = useDownloadStore(selectHasActiveDownloads);

  const [activeTab, setActiveTab] = useState<'pending' | 'finished'>('pending');

  const { message } = App.useApp();
  const { t } = useTranslation();

  // Filter tasks by tab, sort downloading items to top
  const pendingTasks = useMemo(() => {
    const filtered = tasks.filter(
      (t) => t.status === 'pending' || t.status === 'downloading' || t.status === 'paused'
    );
    // Sort: downloading first, then paused, then pending
    return filtered.sort((a, b) => {
      const order: Record<string, number> = { downloading: 0, paused: 1, pending: 2 };
      return (order[a.status] ?? 3) - (order[b.status] ?? 3);
    });
  }, [tasks]);

  const finishedTasks = useMemo(
    () =>
      tasks.filter(
        (t) => t.status === 'success' || t.status === 'error' || t.status === 'cancelled'
      ),
    [tasks]
  );

  // Count for tabs (pending tab = pending + downloading + paused)
  const pendingTabCount = pendingCount + downloadingCount + pausedCount;

  // Refresh tasks from database (background, no loading spinner)
  const reloadTasksFromDatabase = useCallback(async () => {
    if (!storageConfig?.bucket || !storageConfig?.accountId) return;
    try {
      const sessions = await invoke<DownloadSession[]>('get_download_tasks', {
        bucket: storageConfig.bucket,
        accountId: storageConfig.accountId,
      });
      loadFromDatabase(sessions);
    } catch (e) {
      console.error('Failed to reload download tasks:', e);
    }
  }, [storageConfig?.bucket, storageConfig?.accountId, loadFromDatabase]);

  // Refresh tasks in background when modal opens (no loading spinner - show existing data immediately)
  useEffect(() => {
    if (modalOpen && storageConfig?.bucket && storageConfig?.accountId) {
      reloadTasksFromDatabase();
    }
  }, [modalOpen, reloadTasksFromDatabase, storageConfig?.bucket, storageConfig?.accountId]);

  const handleClose = () => {
    setModalOpen(false);
  };

  // Pause all downloads via Rust backend
  const handlePauseAll = async () => {
    if (!storageConfig?.bucket || !storageConfig?.accountId) return;

    try {
      const count = await invoke<number>('pause_all_downloads', {
        bucket: storageConfig.bucket,
        accountId: storageConfig.accountId,
      });
      // Reload immediately to update UI - don't rely solely on async event
      await reloadTasksFromDatabase();
      message.success(t('downloads.paused', { count }));
    } catch (e) {
      console.error('Failed to pause downloads:', e);
      message.error(t('downloads.pauseFailed'));
    }
  };

  // Start/resume all paused and pending downloads via Rust backend
  const handleStartAll = async () => {
    if (
      !storageConfig?.accessKeyId ||
      !storageConfig?.secretAccessKey ||
      (storageConfig.provider === 'aws' && !storageConfig.region) ||
      ((storageConfig.provider === 'minio' || storageConfig.provider === 'rustfs') &&
        (!storageConfig.endpointHost || !storageConfig.endpointScheme))
    ) {
      message.error(t('downloads.credsStart'));
      return;
    }

    try {
      const resumedCount = await invoke<number>('start_all_downloads', {
        config: {
          provider: storageConfig.provider,
          account_id: storageConfig.accountId,
          bucket: storageConfig.bucket,
          access_key_id: storageConfig.accessKeyId,
          secret_access_key: storageConfig.secretAccessKey,
          region: storageConfig.provider === 'aws' ? storageConfig.region : null,
          endpoint_scheme: storageConfig.provider !== 'r2' ? storageConfig.endpointScheme : null,
          endpoint_host: storageConfig.provider !== 'r2' ? storageConfig.endpointHost : null,
          force_path_style: storageConfig.provider === 'r2' ? null : storageConfig.forcePathStyle,
        },
      });
      // Reload immediately to update UI - don't rely solely on async event
      await reloadTasksFromDatabase();
      if (resumedCount > 0) {
        message.success(t('downloads.resumed', { count: resumedCount }));
      } else {
        message.success(t('downloads.starting'));
      }
    } catch (e) {
      console.error('Failed to start downloads:', e);
      message.error(t('downloads.startFailed'));
    }
  };

  // Resume a single paused download (events will update UI)
  const handleResume = async (taskId: string) => {
    if (
      !storageConfig?.accessKeyId ||
      !storageConfig?.secretAccessKey ||
      (storageConfig.provider === 'aws' && !storageConfig.region) ||
      ((storageConfig.provider === 'minio' || storageConfig.provider === 'rustfs') &&
        (!storageConfig.endpointHost || !storageConfig.endpointScheme))
    ) {
      message.error(t('downloads.credsResume'));
      return;
    }

    try {
      // Update status in database (emits download-status-changed event)
      await invoke('resume_download', { taskId });

      // Start download queue to pick up this task
      await invoke('start_download_queue', {
        config: {
          provider: storageConfig.provider,
          account_id: storageConfig.accountId,
          bucket: storageConfig.bucket,
          access_key_id: storageConfig.accessKeyId,
          secret_access_key: storageConfig.secretAccessKey,
          region: storageConfig.provider === 'aws' ? storageConfig.region : null,
          endpoint_scheme: storageConfig.provider !== 'r2' ? storageConfig.endpointScheme : null,
          endpoint_host: storageConfig.provider !== 'r2' ? storageConfig.endpointHost : null,
          force_path_style: storageConfig.provider === 'r2' ? null : storageConfig.forcePathStyle,
        },
      });
      // UI will update via download-status-changed events
    } catch (e) {
      console.error('Failed to resume download:', e);
      message.error(t('downloads.resumeFailed'));
    }
  };

  // Clear finished tasks via Rust backend
  const handleClearFinished = async () => {
    if (!storageConfig?.bucket || !storageConfig?.accountId) return;

    try {
      await invoke('clear_finished_downloads', {
        bucket: storageConfig.bucket,
        accountId: storageConfig.accountId,
      });
      // Reload immediately to update UI
      await reloadTasksFromDatabase();
    } catch (e) {
      console.error('Failed to clear finished downloads:', e);
      message.error(t('downloads.clearFinishedFailed'));
    }
  };

  // Clear all tasks via Rust backend
  const handleClearAll = async () => {
    if (hasActiveDownloads) return;
    if (!storageConfig?.bucket || !storageConfig?.accountId) return;

    try {
      await invoke('clear_all_downloads', {
        bucket: storageConfig.bucket,
        accountId: storageConfig.accountId,
      });
      // Reload immediately to update UI
      await reloadTasksFromDatabase();
    } catch (e) {
      console.error('Failed to clear all downloads:', e);
      message.error(t('downloads.clearAllFailed'));
    }
  };

  // Aggregate stats for the header
  const totalSpeed = tasks
    .filter((t) => t.status === 'downloading')
    .reduce((sum, t) => sum + t.speed, 0);
  // Include ALL pending work: downloading (partial remaining) + queued (full size) + paused (partial remaining)
  const totalRemaining = tasks
    .filter((t) => t.status === 'downloading' || t.status === 'pending' || t.status === 'paused')
    .reduce((sum, t) => sum + Math.max(0, t.fileSize - t.downloadedBytes), 0);
  const etaSeconds = totalSpeed > 0 ? totalRemaining / totalSpeed : 0;

  // Render task list for a tab using Virtuoso
  const renderTaskList = (taskList: DownloadTask[], emptyText: string) => {
    if (taskList.length === 0) {
      return <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={emptyText} />;
    }
    return (
      <Virtuoso
        className="download-task-list"
        style={{ height: 340 }}
        data={taskList}
        itemContent={(index, task) => (
          <DownloadTaskItem key={task.id} task={task} onResume={() => handleResume(task.id)} />
        )}
      />
    );
  };

  // Tab items with counts
  const tabItems = [
    {
      key: 'pending',
      label: (
        <span>
          {t('tasks.pending')}{' '}
          <Badge
            count={pendingTabCount}
            showZero
            size="small"
            style={{
              backgroundColor:
                pendingTabCount > 0 ? 'var(--color-link)' : 'var(--color-border-control-hover)',
            }}
          />
        </span>
      ),
      children: (
        <>
          {/* Aggregate stats header when downloads are active */}
          {downloadingCount > 0 && (
            <div
              style={{
                padding: '6px 12px',
                marginBottom: 8,
                borderRadius: 6,
                backgroundColor: 'var(--color-bg-surface)',
                border: '1px solid var(--color-border-subtle)',
                display: 'flex',
                gap: 12,
                fontSize: 12,
                color: 'var(--color-text-secondary)',
              }}
            >
              {totalSpeed > 0 && <span>{formatBytes(totalSpeed)}/s</span>}
              <span>
                {t('downloads.downloading', { count: downloadingCount })}
                {pendingCount > 0 ? ` · ${t('transfer.queued', { count: pendingCount })}` : ''}
              </span>
              {etaSeconds > 0 && <span>{formatEta(etaSeconds)}</span>}
            </div>
          )}
          {renderTaskList(pendingTasks, t('downloads.emptyPending'))}
        </>
      ),
    },
    {
      key: 'finished',
      label: (
        <span>
          {t('tasks.finished')}{' '}
          <Badge
            count={finishedCount}
            showZero
            size="small"
            style={{
              backgroundColor:
                finishedCount > 0 ? 'var(--color-success)' : 'var(--color-border-control-hover)',
            }}
          />
        </span>
      ),
      children: renderTaskList(finishedTasks, t('downloads.emptyFinished')),
    },
  ];

  return (
    <Modal
      title={t('downloads.title')}
      open={modalOpen}
      onCancel={handleClose}
      destroyOnHidden={false}
      focusable={focusable}
      footer={
        tasks.length > 0 ? (
          <div style={{ display: 'flex', justifyContent: 'space-between' }}>
            <div style={{ display: 'flex', gap: 8 }}>
              {activeTab === 'finished' && finishedCount > 0 && (
                <Button onClick={handleClearFinished} size="small">
                  {t('common.clearFinished')}
                </Button>
              )}
              {activeTab === 'pending' && (downloadingCount > 0 || pendingCount > 0) && (
                <Button onClick={handlePauseAll} size="small">
                  {t('tasks.pauseAll')}
                </Button>
              )}
              {activeTab === 'pending' && (pausedCount > 0 || pendingCount > 0) && (
                <Button onClick={handleStartAll} size="small" type="primary">
                  {pausedCount > 0 ? t('tasks.resumeAll') : t('tasks.startAll')}
                </Button>
              )}
            </div>
            <div>
              {!hasActiveDownloads && tasks.length > 0 && (
                <Popconfirm
                  title={t('downloads.clearAllTitle')}
                  description={t('downloads.clearAllDesc')}
                  onConfirm={handleClearAll}
                  okText={t('common.clearAll')}
                  cancelText={t('common.cancel')}
                  okButtonProps={{ danger: true }}
                >
                  <Button size="small" danger>
                    {t('common.clearAll')}
                  </Button>
                </Popconfirm>
              )}
              <Button onClick={handleClose} style={{ marginLeft: 8 }}>
                {t('common.close')}
              </Button>
            </div>
          </div>
        ) : null
      }
      width={600}
    >
      <div className="cursor-default select-none">
        <Tabs
          activeKey={activeTab}
          onChange={(key) => setActiveTab(key as 'pending' | 'finished')}
          items={tabItems}
          size="small"
        />
      </div>
    </Modal>
  );
}
