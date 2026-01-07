'use client';

import { useEffect, useState, useMemo, useCallback } from 'react';
import { Modal, Button, Empty, App, Tabs, Badge, Spin } from 'antd';
import { invoke } from '@tauri-apps/api/core';
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
} from '../stores/downloadStore';
import DownloadTaskItem from './DownloadTaskItem';

interface DownloadTaskModalProps {
  currentConfig?: {
    account_id: string;
    bucket: string;
    access_key_id?: string | null;
    secret_access_key?: string | null;
  } | null;
}

export default function DownloadTaskModal({ currentConfig }: DownloadTaskModalProps) {
  const tasks = useDownloadStore((state) => state.tasks);
  const modalOpen = useDownloadStore((state) => state.modalOpen);
  const setModalOpen = useDownloadStore((state) => state.setModalOpen);
  const loadFromDatabase = useDownloadStore((state) => state.loadFromDatabase);

  const pendingCount = useDownloadStore(selectPendingCount);
  const downloadingCount = useDownloadStore(selectDownloadingCount);
  const pausedCount = useDownloadStore(selectPausedCount);
  const finishedCount = useDownloadStore(selectFinishedCount);
  const hasActiveDownloads = useDownloadStore(selectHasActiveDownloads);

  const [activeTab, setActiveTab] = useState<'pending' | 'finished'>('pending');
  const [loading, setLoading] = useState(false);

  const { message } = App.useApp();

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

  // Reload tasks from database (only used on modal open for initial sync)
  const reloadTasksFromDatabase = useCallback(
    async (showLoading = false) => {
      if (!currentConfig?.bucket || !currentConfig?.account_id) return;
      if (showLoading) setLoading(true);
      try {
        const sessions = await invoke<DownloadSession[]>('get_download_tasks', {
          bucket: currentConfig.bucket,
          accountId: currentConfig.account_id,
        });
        loadFromDatabase(sessions);
      } catch (e) {
        console.error('Failed to reload download tasks:', e);
      } finally {
        if (showLoading) setLoading(false);
      }
    },
    [currentConfig?.bucket, currentConfig?.account_id, loadFromDatabase]
  );

  // Reload tasks from database when modal opens
  useEffect(() => {
    if (modalOpen && currentConfig?.bucket && currentConfig?.account_id) {
      reloadTasksFromDatabase(true);
    }
  }, [modalOpen, reloadTasksFromDatabase]);

  const handleClose = () => {
    setModalOpen(false);
  };

  // Pause all downloads via Rust backend
  const handlePauseAll = async () => {
    if (!currentConfig?.bucket || !currentConfig?.account_id) return;

    try {
      const count = await invoke<number>('pause_all_downloads', {
        bucket: currentConfig.bucket,
        accountId: currentConfig.account_id,
      });
      // Reload immediately to update UI - don't rely solely on async event
      await reloadTasksFromDatabase();
      message.success(`Paused ${count} downloads`);
    } catch (e) {
      console.error('Failed to pause downloads:', e);
      message.error('Failed to pause downloads');
    }
  };

  // Start/resume all paused and pending downloads via Rust backend
  const handleStartAll = async () => {
    if (!currentConfig?.access_key_id || !currentConfig?.secret_access_key) {
      message.error('S3 credentials required to start downloads');
      return;
    }

    try {
      const resumedCount = await invoke<number>('start_all_downloads', {
        bucket: currentConfig.bucket,
        accountId: currentConfig.account_id,
        accessKeyId: currentConfig.access_key_id,
        secretAccessKey: currentConfig.secret_access_key,
      });
      // Reload immediately to update UI - don't rely solely on async event
      await reloadTasksFromDatabase();
      if (resumedCount > 0) {
        message.success(`Resumed ${resumedCount} downloads`);
      } else {
        message.success('Starting downloads');
      }
    } catch (e) {
      console.error('Failed to start downloads:', e);
      message.error('Failed to start downloads');
    }
  };

  // Resume a single paused download (events will update UI)
  const handleResume = async (taskId: string) => {
    if (!currentConfig?.access_key_id || !currentConfig?.secret_access_key) {
      message.error('S3 credentials required to resume download');
      return;
    }

    try {
      // Update status in database (emits download-status-changed event)
      await invoke('resume_download', { taskId });

      // Start download queue to pick up this task
      await invoke('start_download_queue', {
        bucket: currentConfig.bucket,
        accountId: currentConfig.account_id,
        accessKeyId: currentConfig.access_key_id,
        secretAccessKey: currentConfig.secret_access_key,
      });
      // UI will update via download-status-changed events
    } catch (e) {
      console.error('Failed to resume download:', e);
      message.error('Failed to resume download');
    }
  };

  // Clear finished tasks via Rust backend
  const handleClearFinished = async () => {
    if (!currentConfig?.bucket || !currentConfig?.account_id) return;

    try {
      await invoke('clear_finished_downloads', {
        bucket: currentConfig.bucket,
        accountId: currentConfig.account_id,
      });
      // Reload immediately to update UI
      await reloadTasksFromDatabase();
    } catch (e) {
      console.error('Failed to clear finished downloads:', e);
      message.error('Failed to clear finished downloads');
    }
  };

  // Clear all tasks via Rust backend
  const handleClearAll = async () => {
    if (hasActiveDownloads) return;
    if (!currentConfig?.bucket || !currentConfig?.account_id) return;

    try {
      await invoke('clear_all_downloads', {
        bucket: currentConfig.bucket,
        accountId: currentConfig.account_id,
      });
      // Reload immediately to update UI
      await reloadTasksFromDatabase();
    } catch (e) {
      console.error('Failed to clear all downloads:', e);
      message.error('Failed to clear all downloads');
    }
  };

  // Render task list for a tab using Virtuoso
  const renderTaskList = (taskList: DownloadTask[]) => {
    if (loading) {
      return (
        <div
          style={{ height: 350, display: 'flex', alignItems: 'center', justifyContent: 'center' }}
        >
          <Spin tip="Loading..." fullscreen />
        </div>
      );
    }
    if (taskList.length === 0) {
      return <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="No downloads" />;
    }
    return (
      <Virtuoso
        className="download-task-list"
        style={{ height: 350 }}
        data={taskList}
        itemContent={(index, task) => (
          <DownloadTaskItem
            key={task.id}
            task={task}
            onResume={() => handleResume(task.id)}
            currentConfig={currentConfig}
          />
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
          Pending{' '}
          <Badge
            count={pendingTabCount}
            showZero
            size="small"
            style={{ backgroundColor: pendingTabCount > 0 ? '#1677ff' : '#d9d9d9' }}
          />
        </span>
      ),
      children: renderTaskList(pendingTasks),
    },
    {
      key: 'finished',
      label: (
        <span>
          Finished{' '}
          <Badge
            count={finishedCount}
            showZero
            size="small"
            style={{ backgroundColor: finishedCount > 0 ? '#52c41a' : '#d9d9d9' }}
          />
        </span>
      ),
      children: renderTaskList(finishedTasks),
    },
  ];

  return (
    <Modal
      title="Downloads"
      open={modalOpen}
      onCancel={handleClose}
      footer={
        tasks.length > 0 ? (
          <div style={{ display: 'flex', justifyContent: 'space-between' }}>
            <div style={{ display: 'flex', gap: 8 }}>
              {activeTab === 'finished' && finishedCount > 0 && (
                <Button onClick={handleClearFinished} size="small">
                  Clear Finished
                </Button>
              )}
              {activeTab === 'pending' && (downloadingCount > 0 || pendingCount > 0) && (
                <Button onClick={handlePauseAll} size="small">
                  Pause All
                </Button>
              )}
              {activeTab === 'pending' && (pausedCount > 0 || pendingCount > 0) && (
                <Button onClick={handleStartAll} size="small" type="primary">
                  {pausedCount > 0 ? 'Resume All' : 'Start All'}
                </Button>
              )}
            </div>
            <div>
              {!hasActiveDownloads && tasks.length > 0 && (
                <Button onClick={handleClearAll} size="small" danger>
                  Clear All
                </Button>
              )}
              <Button onClick={handleClose} style={{ marginLeft: 8 }}>
                Close
              </Button>
            </div>
          </div>
        ) : null
      }
      width={500}
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
