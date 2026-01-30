'use client';

import { useEffect, useMemo, useRef, useState } from 'react';
import { Spin, Typography } from 'antd';
import {
  SwapOutlined,
  CheckCircleOutlined,
  PauseCircleOutlined,
  ClockCircleOutlined,
} from '@ant-design/icons';
import { invoke } from '@tauri-apps/api/core';
import {
  useMoveStore,
  selectActiveCount,
  selectDownloadingCount,
  selectUploadingCount,
  selectFinishingCount,
  selectPendingCount,
  selectPausedCount,
  selectFinishedCount,
  selectHasActiveMoves,
  MAX_CONCURRENT_MOVES,
  MoveSession,
  setupGlobalMoveListeners,
  loadAllActiveMoves,
} from '@/app/stores/moveStore';
import { formatBytes } from '@/app/utils/formatBytes';

const { Text } = Typography;

interface MoveProgressProps {
  sourceBucket?: string;
  sourceAccountId?: string;
}

export default function MoveProgress({ sourceBucket, sourceAccountId }: MoveProgressProps) {
  const tasks = useMoveStore((state) => state.tasks);
  const activeCount = useMoveStore(selectActiveCount);
  const downloadingCount = useMoveStore(selectDownloadingCount);
  const uploadingCount = useMoveStore(selectUploadingCount);
  const finishingCount = useMoveStore(selectFinishingCount);
  const pendingCount = useMoveStore(selectPendingCount);
  const pausedCount = useMoveStore(selectPausedCount);
  const finishedCount = useMoveStore(selectFinishedCount);
  const hasActiveMoves = useMoveStore(selectHasActiveMoves);
  const setModalOpen = useMoveStore((state) => state.setModalOpen);
  const loadFromDatabase = useMoveStore((state) => state.loadFromDatabase);
  const [syncSpeed, setSyncSpeed] = useState(0);
  const lastUpdateRef = useRef(0);
  const speedSamplesRef = useRef(new Map<string, { bytes: number; time: number; speed: number }>());

  // Setup global listeners once (persists even when component unmounts)
  useEffect(() => {
    setupGlobalMoveListeners();
    loadAllActiveMoves();
  }, []);

  // Load current account's tasks when switching accounts (to get finished tasks)
  useEffect(() => {
    if (!sourceBucket || !sourceAccountId) return;

    const loadAccountTasks = async () => {
      try {
        const sessions = await invoke<MoveSession[]>('get_move_tasks', {
          sourceBucket,
          sourceAccountId,
        });
        if (sessions.length > 0) {
          loadFromDatabase(sessions);
        }
      } catch (e) {
        console.error('Failed to load account move tasks:', e);
      }
    };

    loadAccountTasks();
  }, [sourceBucket, sourceAccountId, loadFromDatabase]);

  const activeTransferTasks = useMemo(
    () =>
      tasks.filter(
        (t) =>
          t.status === 'downloading' || (t.status === 'uploading' && t.progress < 100)
      ),
    [tasks]
  );

  useEffect(() => {
    if (activeTransferTasks.length === 0) {
      setSyncSpeed(0);
      lastUpdateRef.current = 0;
      speedSamplesRef.current.clear();
      return;
    }

    const now = Date.now();
    const activeIds = new Set(activeTransferTasks.map((task) => task.id));
    for (const taskId of speedSamplesRef.current.keys()) {
      if (!activeIds.has(taskId)) {
        speedSamplesRef.current.delete(taskId);
      }
    }

    let totalSpeed = 0;
    for (const task of activeTransferTasks) {
      const prevSample = speedSamplesRef.current.get(task.id);
      if (prevSample && now > prevSample.time) {
        const deltaBytes = Math.max(0, task.transferredBytes - prevSample.bytes);
        const deltaSeconds = (now - prevSample.time) / 1000;
        const calculatedSpeed = deltaSeconds > 0 ? deltaBytes / deltaSeconds : 0;
        const nextSpeed = calculatedSpeed > 0 ? calculatedSpeed : task.speed || prevSample.speed || 0;
        speedSamplesRef.current.set(task.id, {
          bytes: task.transferredBytes,
          time: now,
          speed: nextSpeed,
        });
        totalSpeed += nextSpeed;
      } else {
        const seededSpeed = task.speed || 0;
        speedSamplesRef.current.set(task.id, {
          bytes: task.transferredBytes,
          time: now,
          speed: seededSpeed,
        });
        totalSpeed += seededSpeed;
      }
    }

    setSyncSpeed(totalSpeed);
    lastUpdateRef.current = now;
  }, [activeTransferTasks]);

  useEffect(() => {
    const interval = setInterval(() => {
      if (lastUpdateRef.current > 0 && Date.now() - lastUpdateRef.current > 1000) {
        setSyncSpeed(0);
      }
    }, 1000);
    return () => clearInterval(interval);
  }, []);

  if (tasks.length === 0) {
    return null;
  }

  return (
    <span
      className="move-progress"
      onClick={() => setModalOpen(true)}
      style={{ cursor: 'pointer' }}
      title="Click to view move details"
    >
      {hasActiveMoves ? (
        <>
          <Spin size="small" />
          <SwapOutlined style={{ marginLeft: 4 }} />
          <Text style={{ marginLeft: 4 }}>
            {activeCount}/{MAX_CONCURRENT_MOVES} moving
            {(downloadingCount > 0 || uploadingCount > 0) &&
              ` (${downloadingCount} down, ${uploadingCount} up)`}
            {finishingCount > 0 && `, ${finishingCount} finishing`}
            {pendingCount > 0 && ` (${pendingCount} queued)`}
          </Text>
          {syncSpeed > 0 && (
            <Text type="secondary" style={{ marginLeft: 4 }}>
              {formatBytes(syncSpeed)}/s
            </Text>
          )}
        </>
      ) : finishingCount > 0 ? (
        // Finishing phase (upload at 100% or deleting) - doesn't block new transfers
        <>
          <SwapOutlined style={{ color: '#52c41a' }} />
          <Text style={{ marginLeft: 4 }}>
            {finishingCount} finishing
            {pendingCount > 0 && ` (${pendingCount} queued)`}
          </Text>
        </>
      ) : pendingCount > 0 ? (
        <>
          <ClockCircleOutlined style={{ color: '#1677ff' }} />
          <Text style={{ marginLeft: 4 }}>{pendingCount} queued</Text>
        </>
      ) : pausedCount > 0 ? (
        <>
          <PauseCircleOutlined style={{ color: '#faad14' }} />
          <Text style={{ marginLeft: 4 }}>{pausedCount} paused</Text>
        </>
      ) : (
        <>
          <CheckCircleOutlined style={{ color: '#52c41a' }} />
          <Text style={{ marginLeft: 4 }}>{finishedCount} moved</Text>
        </>
      )}
    </span>
  );
}
