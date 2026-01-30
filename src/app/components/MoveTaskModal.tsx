'use client';

import { useEffect, useState, useMemo, useCallback } from 'react';
import { Modal, Button, Empty, App, Tabs, Badge } from 'antd';
import { invoke } from '@tauri-apps/api/core';
import { Virtuoso } from 'react-virtuoso';
import {
  useMoveStore,
  selectPendingCount,
  selectActiveCount,
  selectDownloadingCount,
  selectUploadingCount,
  selectFinishingCount,
  selectPausedCount,
  selectFinishedCount,
  selectHasInProgressMoves,
  MoveSession,
  MoveTask,
} from '@/app/stores/moveStore';
import { useAccountStore } from '@/app/stores/accountStore';
import MoveTaskItem from '@/app/components/MoveTaskItem';
import type { StorageConfig } from '@/app/lib/r2cache';

interface MoveTaskModalProps {
  storageConfig?: StorageConfig | null;
}

interface MoveConfigInput {
  provider: string;
  account_id: string;
  bucket: string;
  access_key_id: string;
  secret_access_key: string;
  region?: string | null;
  endpoint_scheme?: string | null;
  endpoint_host?: string | null;
  force_path_style?: boolean | null;
}

export default function MoveTaskModal({ storageConfig }: MoveTaskModalProps) {
  const tasks = useMoveStore((state) => state.tasks);
  const modalOpen = useMoveStore((state) => state.modalOpen);
  const setModalOpen = useMoveStore((state) => state.setModalOpen);
  const loadFromDatabase = useMoveStore((state) => state.loadFromDatabase);

  const pendingCount = useMoveStore(selectPendingCount);
  const activeCount = useMoveStore(selectActiveCount);
  const downloadingCount = useMoveStore(selectDownloadingCount);
  const uploadingCount = useMoveStore(selectUploadingCount);
  const finishingCount = useMoveStore(selectFinishingCount);
  const pausedCount = useMoveStore(selectPausedCount);
  const finishedCount = useMoveStore(selectFinishedCount);
  const hasInProgressMoves = useMoveStore(selectHasInProgressMoves);

  const accounts = useAccountStore((state) => state.accounts);
  const loadAccounts = useAccountStore((state) => state.loadAccounts);

  const [activeTab, setActiveTab] = useState<'pending' | 'finishing' | 'finished'>('pending');

  const { message } = App.useApp();

  // In Progress: tasks actively transferring or waiting (excludes finishing)
  const inProgressTasks = useMemo(() => {
    const filtered = tasks.filter(
      (t) =>
        t.status === 'pending' ||
        t.status === 'downloading' ||
        (t.status === 'uploading' && t.progress < 100) ||
        t.status === 'paused'
    );
    // Sort order: active transfers first, then paused, then pending
    const getSortOrder = (task: MoveTask): number => {
      if (task.status === 'downloading') return 0;
      if (task.status === 'uploading') return 1;
      if (task.status === 'paused') return 2;
      if (task.status === 'pending') return 3;
      return 10;
    };
    return filtered.sort((a, b) => getSortOrder(a) - getSortOrder(b));
  }, [tasks]);

  // Finishing: tasks completing in background (upload at 100% or deleting)
  const finishingTasks = useMemo(() => {
    const filtered = tasks.filter(
      (t) =>
        t.status === 'finishing' ||
        t.status === 'deleting' ||
        (t.status === 'uploading' && t.progress >= 100)
    );
    // Sort by status: deleting first, then uploading at 100%
    return filtered.sort((a, b) => {
      if (a.status === 'deleting' && b.status !== 'deleting') return -1;
      if (a.status !== 'deleting' && b.status === 'deleting') return 1;
      return 0;
    });
  }, [tasks]);

  const finishedTasks = useMemo(
    () => tasks.filter((t) => t.status === 'success' || t.status === 'error' || t.status === 'cancelled'),
    [tasks]
  );

  // Count for In Progress tab (excludes finishing)
  const inProgressTabCount = pendingCount + activeCount + pausedCount;

  const reloadTasksFromDatabase = useCallback(async () => {
    if (!storageConfig?.bucket || !storageConfig?.accountId) return;
    try {
      const sessions = await invoke<MoveSession[]>('get_move_tasks', {
        sourceBucket: storageConfig.bucket,
        sourceAccountId: storageConfig.accountId,
      });
      loadFromDatabase(sessions);
    } catch (e) {
      console.error('Failed to reload move tasks:', e);
    }
  }, [storageConfig?.bucket, storageConfig?.accountId, loadFromDatabase]);

  // Load accounts and refresh tasks in background when modal opens (no loading spinner)
  useEffect(() => {
    if (modalOpen) {
      loadAccounts().catch((e) => console.error('Failed to load accounts:', e));
      reloadTasksFromDatabase();
    }
  }, [modalOpen, loadAccounts, reloadTasksFromDatabase]);

  const handleClose = () => {
    setModalOpen(false);
  };

  const buildSourceConfig = (): MoveConfigInput | null => {
    if (!storageConfig?.accountId || !storageConfig.bucket) return null;
    if (!storageConfig.accessKeyId || !storageConfig.secretAccessKey) return null;
    if (storageConfig.provider === 'aws' && !storageConfig.region) return null;
    if (
      (storageConfig.provider === 'minio' || storageConfig.provider === 'rustfs') &&
      (!storageConfig.endpointScheme || !storageConfig.endpointHost)
    ) {
      return null;
    }

    return {
      provider: storageConfig.provider,
      account_id: storageConfig.accountId,
      bucket: storageConfig.bucket,
      access_key_id: storageConfig.accessKeyId,
      secret_access_key: storageConfig.secretAccessKey,
      region: storageConfig.provider === 'aws' ? storageConfig.region : null,
      endpoint_scheme: storageConfig.provider === 'r2' ? null : storageConfig.endpointScheme,
      endpoint_host: storageConfig.provider === 'r2' ? null : storageConfig.endpointHost,
      force_path_style: storageConfig.provider === 'r2' ? null : storageConfig.forcePathStyle,
    };
  };

  const buildDestinationConfig = (task: MoveTask): MoveConfigInput | null => {
    const accountEntry = accounts.find(
      (account) => account.provider === task.destProvider && account.account.id === task.destAccountId
    );
    if (!accountEntry) return null;

    if (accountEntry.provider === 'r2') {
      const tokenEntry = accountEntry.tokens.find((token) =>
        token.buckets.some((bucket) => bucket.name === task.destBucket)
      );
      if (!tokenEntry) return null;
      return {
        provider: 'r2',
        account_id: accountEntry.account.id,
        bucket: task.destBucket,
        access_key_id: tokenEntry.token.access_key_id,
        secret_access_key: tokenEntry.token.secret_access_key,
        region: null,
        endpoint_scheme: null,
        endpoint_host: null,
        force_path_style: null,
      };
    }

    if (accountEntry.provider === 'aws') {
      return {
        provider: 'aws',
        account_id: accountEntry.account.id,
        bucket: task.destBucket,
        access_key_id: accountEntry.account.access_key_id,
        secret_access_key: accountEntry.account.secret_access_key,
        region: accountEntry.account.region,
        endpoint_scheme: accountEntry.account.endpoint_scheme,
        endpoint_host: accountEntry.account.endpoint_host,
        force_path_style: accountEntry.account.force_path_style,
      };
    }

    if (accountEntry.provider === 'minio') {
      return {
        provider: 'minio',
        account_id: accountEntry.account.id,
        bucket: task.destBucket,
        access_key_id: accountEntry.account.access_key_id,
        secret_access_key: accountEntry.account.secret_access_key,
        endpoint_scheme: accountEntry.account.endpoint_scheme,
        endpoint_host: accountEntry.account.endpoint_host,
        force_path_style: accountEntry.account.force_path_style,
      };
    }

    if (accountEntry.provider === 'rustfs') {
      return {
        provider: 'rustfs',
        account_id: accountEntry.account.id,
        bucket: task.destBucket,
        access_key_id: accountEntry.account.access_key_id,
        secret_access_key: accountEntry.account.secret_access_key,
        endpoint_scheme: accountEntry.account.endpoint_scheme,
        endpoint_host: accountEntry.account.endpoint_host,
        force_path_style: true,
      };
    }

    return null;
  };

  const handlePauseAll = async () => {
    if (!storageConfig?.bucket || !storageConfig?.accountId) return;

    try {
      const count = await invoke<number>('pause_all_moves', {
        sourceBucket: storageConfig.bucket,
        sourceAccountId: storageConfig.accountId,
      });
      await reloadTasksFromDatabase();
      message.success(`Paused ${count} moves`);
    } catch (e) {
      console.error('Failed to pause moves:', e);
      message.error('Failed to pause moves');
    }
  };

  const handleStartAll = async () => {
    const sourceConfig = buildSourceConfig();
    if (!sourceConfig) {
      message.error('Source credentials are required to start moves');
      return;
    }

    try {
      // Resume paused tasks first (changes DB status from 'paused' to 'pending')
      if (pausedCount > 0) {
        await invoke('resume_all_moves', {
          sourceBucket: sourceConfig.bucket,
          sourceAccountId: sourceConfig.account_id,
        });
      }

      // Get tasks to start (pending or paused in local state)
      const pendingTargets = inProgressTasks.filter(
        (task) => task.status === 'pending' || task.status === 'paused'
      );

      // Group by destination
      const groups = new Map<string, { config: MoveConfigInput; tasks: MoveTask[] }>();
      for (const task of pendingTargets) {
        const destConfig = buildDestinationConfig(task);
        if (!destConfig) {
          console.warn('Could not build dest config for task:', task.id, task.destProvider, task.destAccountId);
          continue;
        }
        const groupKey = `${destConfig.provider}:${destConfig.account_id}:${destConfig.bucket}`;
        const existing = groups.get(groupKey);
        if (existing) {
          existing.tasks.push(task);
        } else {
          groups.set(groupKey, { config: destConfig, tasks: [task] });
        }
      }

      if (groups.size === 0) {
        // No groups found - try to reload accounts and retry
        await loadAccounts();
        
        // Re-check with fresh accounts
        const freshGroups = new Map<string, { config: MoveConfigInput; tasks: MoveTask[] }>();
        for (const task of pendingTargets) {
          const destConfig = buildDestinationConfig(task);
          if (!destConfig) continue;
          const groupKey = `${destConfig.provider}:${destConfig.account_id}:${destConfig.bucket}`;
          const existing = freshGroups.get(groupKey);
          if (existing) {
            existing.tasks.push(task);
          } else {
            freshGroups.set(groupKey, { config: destConfig, tasks: [task] });
          }
        }

        if (freshGroups.size === 0) {
          message.error('No destination credentials available. Please add the destination account first.');
          return;
        }

        // Use fresh groups
        for (const group of freshGroups.values()) {
          await invoke('start_move_queue', {
            sourceConfig,
            destConfig: group.config,
          });
        }
      } else {
        // Start each group
        for (const group of groups.values()) {
          await invoke('start_move_queue', {
            sourceConfig,
            destConfig: group.config,
          });
        }
      }

      await reloadTasksFromDatabase();
      message.success('Starting moves');
    } catch (e) {
      console.error('Failed to start moves:', e);
      message.error('Failed to start moves');
    }
  };

  const handleResume = async (taskId: string) => {
    const sourceConfig = buildSourceConfig();
    if (!sourceConfig) {
      message.error('Source credentials are required to resume moves');
      return;
    }

    const task = tasks.find((item) => item.id === taskId);
    if (!task) return;

    const destConfig = buildDestinationConfig(task);
    if (!destConfig) {
      message.error('Destination credentials are required to resume this move');
      return;
    }

    try {
      await invoke('resume_move', { taskId });
      await invoke('start_move_queue', {
        sourceConfig,
        destConfig,
      });
    } catch (e) {
      console.error('Failed to resume move:', e);
      message.error('Failed to resume move');
    }
  };

  const handleClearFinished = async () => {
    if (!storageConfig?.bucket || !storageConfig?.accountId) return;

    try {
      await invoke('clear_finished_moves', {
        sourceBucket: storageConfig.bucket,
        sourceAccountId: storageConfig.accountId,
      });
      await reloadTasksFromDatabase();
    } catch (e) {
      console.error('Failed to clear finished moves:', e);
      message.error('Failed to clear finished moves');
    }
  };

  const handleClearAll = async () => {
    // Don't clear if any tasks are in progress (including deleting phase)
    if (hasInProgressMoves) return;
    if (!storageConfig?.bucket || !storageConfig?.accountId) return;

    try {
      await invoke('clear_all_moves', {
        sourceBucket: storageConfig.bucket,
        sourceAccountId: storageConfig.accountId,
      });
      await reloadTasksFromDatabase();
    } catch (e) {
      console.error('Failed to clear all moves:', e);
      message.error('Failed to clear all moves');
    }
  };

  const renderTaskList = (taskList: MoveTask[]) => {
    if (taskList.length === 0) {
      return <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="No moves" />;
    }
    return (
      <Virtuoso
        className="move-task-list"
        style={{ height: 350 }}
        data={taskList}
        itemContent={(index, task) => (
          <MoveTaskItem key={task.id} task={task} onResume={() => handleResume(task.id)} />
        )}
      />
    );
  };

  const tabItems = [
    {
      key: 'pending',
      label: (
        <span>
          In Progress{' '}
          <Badge
            count={inProgressTabCount}
            showZero
            size="small"
            style={{ backgroundColor: inProgressTabCount > 0 ? '#1677ff' : '#d9d9d9' }}
          />
          {(downloadingCount > 0 || uploadingCount > 0) && (
            <span style={{ marginLeft: 6, color: '#8c8c8c', fontSize: 12 }}>
              ({downloadingCount} down, {uploadingCount} up)
            </span>
          )}
        </span>
      ),
      children: renderTaskList(inProgressTasks),
    },
    {
      key: 'finishing',
      label: (
        <span>
          Finishing{' '}
          <Badge
            count={finishingCount}
            showZero
            size="small"
            style={{ backgroundColor: finishingCount > 0 ? '#fa8c16' : '#d9d9d9' }}
          />
        </span>
      ),
      children: renderTaskList(finishingTasks),
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
      title="Moves"
      open={modalOpen}
      onCancel={handleClose}
      destroyOnHidden={false}
      footer={
        tasks.length > 0 ? (
          <div style={{ display: 'flex', justifyContent: 'space-between' }}>
            <div style={{ display: 'flex', gap: 8 }}>
              {activeTab === 'finished' && finishedCount > 0 && (
                <Button onClick={handleClearFinished} size="small">
                  Clear Finished
                </Button>
              )}
              {activeTab === 'pending' && (activeCount > 0 || pendingCount > 0) && (
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
              {!hasInProgressMoves && tasks.length > 0 && (
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
      width={560}
    >
      <div className="cursor-default select-none">
        <Tabs
          activeKey={activeTab}
          onChange={(key) => setActiveTab(key as 'pending' | 'finishing' | 'finished')}
          items={tabItems}
          size="small"
        />
      </div>
    </Modal>
  );
}
