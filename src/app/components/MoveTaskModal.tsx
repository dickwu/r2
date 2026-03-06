'use client';

import { useEffect, useState, useMemo, useCallback } from 'react';
import { Modal, Button, Empty, App, Tabs, Badge, Popconfirm } from 'antd';
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
import { useAccountStore, type ProviderAccount } from '@/app/stores/accountStore';
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

function buildMoveConfigFromAccounts(
  provider: string,
  accountId: string,
  bucket: string,
  accounts: ProviderAccount[]
): MoveConfigInput | null {
  const accountEntry = accounts.find(
    (account) => account.provider === provider && account.account.id === accountId
  );
  if (!accountEntry) return null;

  if (accountEntry.provider === 'r2') {
    const tokenEntry = accountEntry.tokens.find((token) =>
      token.buckets.some((item) => item.name === bucket)
    );
    if (!tokenEntry) return null;
    return {
      provider: 'r2',
      account_id: accountEntry.account.id,
      bucket,
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
      bucket,
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
      bucket,
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
      bucket,
      access_key_id: accountEntry.account.access_key_id,
      secret_access_key: accountEntry.account.secret_access_key,
      endpoint_scheme: accountEntry.account.endpoint_scheme,
      endpoint_host: accountEntry.account.endpoint_host,
      force_path_style: true,
    };
  }

  return null;
}

function buildDestinationConfigFromAccounts(
  task: MoveTask,
  accounts: ProviderAccount[]
): MoveConfigInput | null {
  return buildMoveConfigFromAccounts(
    task.destProvider,
    task.destAccountId,
    task.destBucket,
    accounts
  );
}

function buildSourceConfigFromAccounts(
  task: MoveTask,
  accounts: ProviderAccount[]
): MoveConfigInput | null {
  return buildMoveConfigFromAccounts(
    task.sourceProvider,
    task.sourceAccountId,
    task.sourceBucket,
    accounts
  );
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
    () =>
      tasks.filter(
        (t) => t.status === 'success' || t.status === 'error' || t.status === 'cancelled'
      ),
    [tasks]
  );

  // Count for In Progress tab (excludes finishing)
  const inProgressTabCount = pendingCount + activeCount + pausedCount;

  const reloadTasksFromDatabase = useCallback(async () => {
    try {
      // Load global active tasks and merge source history for all known source queues.
      const activeSessions = await invoke<MoveSession[]>('get_all_active_move_tasks');
      const currentTasksSnapshot = useMoveStore.getState().tasks;
      const targets = new Map<string, { sourceBucket: string; sourceAccountId: string }>();

      if (storageConfig?.bucket && storageConfig?.accountId) {
        targets.set(`${storageConfig.accountId}:${storageConfig.bucket}`, {
          sourceBucket: storageConfig.bucket,
          sourceAccountId: storageConfig.accountId,
        });
      }

      for (const task of useMoveStore.getState().tasks) {
        const key = `${task.sourceAccountId}:${task.sourceBucket}`;
        if (!targets.has(key)) {
          targets.set(key, {
            sourceBucket: task.sourceBucket,
            sourceAccountId: task.sourceAccountId,
          });
        }
      }

      const targetList = Array.from(targets.values());
      const sourceResults = await Promise.allSettled(
        targetList.map((target) => invoke<MoveSession[]>('get_move_tasks', target))
      );

      const byId = new Map<string, MoveSession>();
      for (const session of activeSessions) {
        byId.set(session.id, session);
      }
      const failedSourceKeys = new Set<string>();
      for (const [index, result] of sourceResults.entries()) {
        if (result.status === 'fulfilled') {
          for (const session of result.value) {
            byId.set(session.id, session);
          }
          continue;
        }

        const failedTarget = targetList[index];
        if (failedTarget) {
          failedSourceKeys.add(`${failedTarget.sourceAccountId}:${failedTarget.sourceBucket}`);
        }
      }

      // Preserve previously known tasks for sources that failed to reload,
      // so transient per-source fetch failures do not make tasks disappear.
      if (failedSourceKeys.size > 0) {
        for (const task of currentTasksSnapshot) {
          const sourceKey = `${task.sourceAccountId}:${task.sourceBucket}`;
          if (!failedSourceKeys.has(sourceKey) || byId.has(task.id)) continue;

          byId.set(task.id, {
            id: task.id,
            source_key: task.sourceKey,
            dest_key: task.destKey,
            source_bucket: task.sourceBucket,
            source_account_id: task.sourceAccountId,
            source_provider: task.sourceProvider,
            dest_bucket: task.destBucket,
            dest_account_id: task.destAccountId,
            dest_provider: task.destProvider,
            delete_original: task.deleteOriginal,
            file_size: task.fileSize,
            progress: Math.round(task.progress),
            status: task.status,
            error: task.error || null,
            created_at: 0,
            updated_at: Date.now(),
          });
        }
      }

      const mergedSessions = Array.from(byId.values()).sort((a, b) => b.updated_at - a.updated_at);
      loadFromDatabase(mergedSessions);
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

  const collectSourceTargets = useCallback((taskList: MoveTask[]) => {
    const targets = new Map<string, { sourceBucket: string; sourceAccountId: string }>();
    for (const task of taskList) {
      const key = `${task.sourceAccountId}:${task.sourceBucket}`;
      if (!targets.has(key)) {
        targets.set(key, {
          sourceBucket: task.sourceBucket,
          sourceAccountId: task.sourceAccountId,
        });
      }
    }
    return Array.from(targets.values());
  }, []);

  const handlePauseAll = async () => {
    const pauseTargets = new Map<string, { sourceBucket: string; sourceAccountId: string }>();
    for (const task of inProgressTasks) {
      const shouldPause =
        task.status === 'pending' ||
        task.status === 'downloading' ||
        (task.status === 'uploading' && task.progress < 100);

      if (!shouldPause) continue;

      const key = `${task.sourceAccountId}:${task.sourceBucket}`;
      if (!pauseTargets.has(key)) {
        pauseTargets.set(key, {
          sourceBucket: task.sourceBucket,
          sourceAccountId: task.sourceAccountId,
        });
      }
    }

    if (pauseTargets.size === 0) {
      message.info('No active moves to pause');
      return;
    }

    try {
      const results = await Promise.allSettled(
        Array.from(pauseTargets.values()).map((target) => invoke<number>('pause_all_moves', target))
      );

      let pausedCount = 0;
      let failedQueues = 0;
      for (const result of results) {
        if (result.status === 'fulfilled') {
          pausedCount += result.value;
        } else {
          failedQueues += 1;
        }
      }

      await reloadTasksFromDatabase();

      if (failedQueues > 0) {
        message.warning(`Paused ${pausedCount} moves (${failedQueues} queue(s) failed)`);
        return;
      }

      if (pausedCount > 0) {
        message.success(`Paused ${pausedCount} moves`);
      } else {
        message.info('No moves were paused');
      }
    } catch (e) {
      console.error('Failed to pause moves:', e);
      message.error('Failed to pause moves');
    }
  };

  const handleStartAll = async () => {
    try {
      // Get tasks to start (pending or paused in local state)
      const pendingTargets = inProgressTasks.filter(
        (task) => task.status === 'pending' || task.status === 'paused'
      );
      if (pendingTargets.length === 0) {
        message.info('No queued moves to start');
        return;
      }

      const buildStartGroups = (snapshot: ProviderAccount[]) => {
        const groups = new Map<
          string,
          { sourceConfig: MoveConfigInput; destConfig: MoveConfigInput }
        >();
        let unresolvedTasks = 0;

        for (const task of pendingTargets) {
          const sourceConfig = buildSourceConfigFromAccounts(task, snapshot);
          const destConfig = buildDestinationConfigFromAccounts(task, snapshot);

          if (!sourceConfig || !destConfig) {
            unresolvedTasks += 1;
            console.warn(
              'Could not build move configs for task:',
              task.id,
              task.sourceProvider,
              task.sourceAccountId,
              '->',
              task.destProvider,
              task.destAccountId
            );
            continue;
          }

          const groupKey = `${sourceConfig.provider}:${sourceConfig.account_id}:${sourceConfig.bucket}|${destConfig.provider}:${destConfig.account_id}:${destConfig.bucket}`;
          if (!groups.has(groupKey)) {
            groups.set(groupKey, { sourceConfig, destConfig });
          }
        }

        return { groups, unresolvedTasks };
      };

      let { groups, unresolvedTasks } = buildStartGroups(accounts);

      if (groups.size === 0 || unresolvedTasks > 0) {
        // Account store may still be stale (async load + render lag), force a fresh read.
        await loadAccounts();
        const refreshedAccounts = useAccountStore.getState().accounts;
        const rebuilt = buildStartGroups(refreshedAccounts);
        groups = rebuilt.groups;
        unresolvedTasks = rebuilt.unresolvedTasks;
      }

      if (groups.size === 0) {
        message.error('No source/destination credentials available. Please check your accounts.');
        return;
      }

      // Resume paused tasks for all source queues in view.
      const resumeTargets = new Map<string, { sourceBucket: string; sourceAccountId: string }>();
      for (const task of pendingTargets) {
        if (task.status !== 'paused') continue;
        const key = `${task.sourceAccountId}:${task.sourceBucket}`;
        if (!resumeTargets.has(key)) {
          resumeTargets.set(key, {
            sourceBucket: task.sourceBucket,
            sourceAccountId: task.sourceAccountId,
          });
        }
      }

      if (resumeTargets.size > 0) {
        await Promise.allSettled(
          Array.from(resumeTargets.values()).map((target) =>
            invoke<number>('resume_all_moves', target)
          )
        );
      }

      let startedGroups = 0;
      let failedGroups = 0;
      for (const group of groups.values()) {
        try {
          await invoke('start_move_queue', {
            sourceConfig: group.sourceConfig,
            destConfig: group.destConfig,
          });
          startedGroups += 1;
        } catch (error) {
          failedGroups += 1;
          console.error('Failed to start move queue group:', error);
        }
      }

      await reloadTasksFromDatabase();

      if (failedGroups > 0) {
        message.warning(
          `Started ${startedGroups} queue group(s), ${failedGroups} failed` +
            (unresolvedTasks > 0 ? `, ${unresolvedTasks} task(s) missing credentials` : '')
        );
      } else if (unresolvedTasks > 0) {
        message.warning(
          `Starting moves (${unresolvedTasks} task(s) skipped due to missing credentials)`
        );
      } else {
        message.success('Starting moves');
      }
    } catch (e) {
      console.error('Failed to start moves:', e);
      message.error('Failed to start moves');
    }
  };

  const handleResume = async (taskId: string) => {
    const task = tasks.find((item) => item.id === taskId);
    if (!task) return;

    const sourceConfig = buildSourceConfigFromAccounts(task, accounts);
    const destConfig = buildDestinationConfigFromAccounts(task, accounts);
    if (!sourceConfig || !destConfig) {
      await loadAccounts();
      const refreshedAccounts = useAccountStore.getState().accounts;
      const refreshedSourceConfig = buildSourceConfigFromAccounts(task, refreshedAccounts);
      const refreshedDestConfig = buildDestinationConfigFromAccounts(task, refreshedAccounts);

      if (!refreshedSourceConfig || !refreshedDestConfig) {
        message.error('Source/destination credentials are required to resume this move');
        return;
      }

      try {
        await invoke('resume_move', { taskId });
        await invoke('start_move_queue', {
          sourceConfig: refreshedSourceConfig,
          destConfig: refreshedDestConfig,
        });
        return;
      } catch (e) {
        console.error('Failed to resume move:', e);
        message.error('Failed to resume move');
        return;
      }
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
    const targets = collectSourceTargets(finishedTasks);
    if (targets.length === 0) return;

    try {
      const results = await Promise.allSettled(
        targets.map((target) => invoke<number>('clear_finished_moves', target))
      );

      const failedCount = results.filter((result) => result.status === 'rejected').length;
      await reloadTasksFromDatabase();

      if (failedCount === targets.length) {
        message.error('Failed to clear finished moves');
        return;
      }

      if (failedCount > 0) {
        message.warning(`Cleared finished moves (${failedCount} source queue(s) failed)`);
      }
    } catch (e) {
      console.error('Failed to clear finished moves:', e);
      message.error('Failed to clear finished moves');
    }
  };

  const handleClearAll = async () => {
    // Don't clear if any tasks are in progress (including deleting phase)
    if (hasInProgressMoves) return;
    const targets = collectSourceTargets(tasks);
    if (targets.length === 0) return;

    try {
      const results = await Promise.allSettled(
        targets.map((target) => invoke<number>('clear_all_moves', target))
      );

      const failedCount = results.filter((result) => result.status === 'rejected').length;
      await reloadTasksFromDatabase();

      if (failedCount === targets.length) {
        message.error('Failed to clear all moves');
        return;
      }

      if (failedCount > 0) {
        message.warning(`Cleared moves (${failedCount} source queue(s) failed)`);
      }
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
            style={{
              backgroundColor:
                inProgressTabCount > 0 ? 'var(--color-link)' : 'var(--color-border-control-hover)',
            }}
          />
          {(downloadingCount > 0 || uploadingCount > 0) && (
            <span style={{ marginLeft: 6, color: 'var(--color-text-secondary)', fontSize: 12 }}>
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
            style={{
              backgroundColor:
                finishingCount > 0 ? 'var(--color-accent)' : 'var(--color-border-control-hover)',
            }}
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
            style={{
              backgroundColor:
                finishedCount > 0 ? 'var(--color-success)' : 'var(--color-border-control-hover)',
            }}
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
                <Popconfirm
                  title="Clear all moves"
                  description="Are you sure you want to clear all move tasks?"
                  onConfirm={handleClearAll}
                  okText="Clear All"
                  cancelText="Cancel"
                  okButtonProps={{ danger: true }}
                >
                  <Button size="small" danger>
                    Clear All
                  </Button>
                </Popconfirm>
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
