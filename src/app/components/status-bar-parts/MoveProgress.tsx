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
const SPEED_WINDOW_MS = 3000;
const MIN_WINDOW_SPAN_MS = 800;
const SPEED_SMOOTHING_ALPHA = 0.2;
const MAX_SPEED_SPIKE_MULTIPLIER = 3;

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
  const speedSamplesRef = useRef<Array<{ bytes: number; time: number }>>([]);
  const smoothedSpeedRef = useRef(0);

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
        (t) => t.status === 'downloading' || (t.status === 'uploading' && t.progress < 100)
      ),
    [tasks]
  );

  useEffect(() => {
    if (activeTransferTasks.length === 0) {
      setSyncSpeed(0);
      lastUpdateRef.current = 0;
      speedSamplesRef.current = [];
      smoothedSpeedRef.current = 0;
      return;
    }

    const now = Date.now();
    const totalTransferredBytes = activeTransferTasks.reduce(
      (sum, task) => sum + Math.max(0, task.transferredBytes || 0),
      0
    );

    const samples = speedSamplesRef.current;
    if (samples.length > 0 && totalTransferredBytes < samples[samples.length - 1].bytes) {
      // Task list/state reloaded and counters moved backward; restart the window.
      speedSamplesRef.current = [];
    }

    speedSamplesRef.current.push({ bytes: totalTransferredBytes, time: now });

    while (
      speedSamplesRef.current.length > 0 &&
      now - speedSamplesRef.current[0].time > SPEED_WINDOW_MS
    ) {
      speedSamplesRef.current.shift();
    }

    let rawSpeed = 0;
    if (speedSamplesRef.current.length >= 2) {
      const oldest = speedSamplesRef.current[0];
      const deltaBytes = totalTransferredBytes - oldest.bytes;
      const deltaMs = now - oldest.time;
      if (deltaBytes >= 0 && deltaMs >= MIN_WINDOW_SPAN_MS) {
        rawSpeed = deltaBytes / (deltaMs / 1000);
      }
    }

    if (rawSpeed <= 0) {
      // Fallback to backend-reported task speeds when the local window is not yet stable.
      rawSpeed = activeTransferTasks.reduce((sum, task) => sum + Math.max(0, task.speed || 0), 0);
    }

    if (!Number.isFinite(rawSpeed) || rawSpeed < 0) {
      rawSpeed = 0;
    }

    const previous = smoothedSpeedRef.current;
    const cappedRaw =
      previous > 0 ? Math.min(rawSpeed, previous * MAX_SPEED_SPIKE_MULTIPLIER) : rawSpeed;
    const nextSpeed =
      previous > 0
        ? previous * (1 - SPEED_SMOOTHING_ALPHA) + cappedRaw * SPEED_SMOOTHING_ALPHA
        : cappedRaw;

    smoothedSpeedRef.current = nextSpeed;
    setSyncSpeed(nextSpeed);
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
      onClick={() => {
        void loadAllActiveMoves();
        setModalOpen(true);
      }}
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
