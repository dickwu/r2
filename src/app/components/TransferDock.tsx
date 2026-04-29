'use client';

import React, { useState, useEffect, useMemo, useCallback } from 'react';
import { createPortal } from 'react-dom';
import {
  UploadOutlined,
  DownloadOutlined,
  SwapOutlined,
  CheckOutlined,
  CloseOutlined,
  CloseCircleOutlined,
} from '@ant-design/icons';
import { useUploadStore } from '@/app/stores/uploadStore';
import { useDownloadStore } from '@/app/stores/downloadStore';
import { useMoveStore } from '@/app/stores/moveStore';

// How long to keep the dock visible after the last task completes,
// so the user briefly sees the success state before it auto-dismisses.
const AUTO_DISMISS_DELAY_MS = 2500;

// ── Unified task shape ────────────────────────────────────────────────────────

type TaskKind = 'upload' | 'download' | 'move';
type TaskState = 'active' | 'done' | 'error';

interface DockTask {
  id: string;
  kind: TaskKind;
  name: string;
  progress: number;
  state: TaskState;
}

const UPLOAD_TERMINAL = new Set(['success', 'error', 'cancelled']);
const DOWNLOAD_TERMINAL = new Set(['completed', 'failed', 'cancelled']);
const MOVE_TERMINAL = new Set(['success', 'error', 'cancelled']);

function uploadState(status: string): TaskState {
  if (status === 'success') return 'done';
  if (status === 'error') return 'error';
  return 'active';
}

function downloadState(status: string): TaskState {
  if (status === 'completed') return 'done';
  if (status === 'failed') return 'error';
  return 'active';
}

function moveState(status: string): TaskState {
  if (status === 'success') return 'done';
  if (status === 'error') return 'error';
  return 'active';
}

// ── Component ─────────────────────────────────────────────────────────────────

export default function TransferDock() {
  const uploadTasks = useUploadStore((s) => s.tasks);
  const downloadTasks = useDownloadStore((s) => s.tasks);
  const moveTasks = useMoveStore((s) => s.tasks);

  const clearFinishedUploads = useUploadStore((s) => s.clearFinished);
  const clearFinishedDownloads = useDownloadStore((s) => s.clearFinished);
  const clearFinishedMoves = useMoveStore((s) => s.clearFinishedTasks);

  // "dismissed" resets when a NEW task arrives
  const [dismissed, setDismissed] = useState(false);
  const [prevTotal, setPrevTotal] = useState(0);

  const dockTasks = useMemo<DockTask[]>(() => {
    const out: DockTask[] = [];

    for (const t of uploadTasks) {
      out.push({
        id: t.id,
        kind: 'upload',
        name: t.renamedFileName ?? t.fileName,
        progress: t.progress,
        state: uploadState(t.status),
      });
    }
    for (const t of downloadTasks) {
      out.push({
        id: t.id,
        kind: 'download',
        name: t.fileName,
        progress: t.progress,
        state: downloadState(t.status),
      });
    }
    for (const t of moveTasks) {
      const name = t.sourceKey.split('/').filter(Boolean).pop() ?? t.sourceKey;
      out.push({
        id: t.id,
        kind: 'move',
        name,
        progress: t.progress,
        state: moveState(t.status),
      });
    }

    return out;
  }, [uploadTasks, downloadTasks, moveTasks]);

  const total = dockTasks.length;
  const running = dockTasks.filter((t) => t.state === 'active').length;

  // Centralised dismiss path: hide dock + flush finished tasks from stores so
  // they don't reappear alongside the next new task.
  const dismissAndClear = useCallback(() => {
    setDismissed(true);
    clearFinishedUploads();
    clearFinishedDownloads();
    clearFinishedMoves();
  }, [clearFinishedUploads, clearFinishedDownloads, clearFinishedMoves]);

  // Reset dismissed flag whenever a new task arrives
  useEffect(() => {
    if (total > prevTotal) {
      setDismissed(false);
    }
    setPrevTotal(total);
  }, [total, prevTotal]);

  // Auto-dismiss once every task has reached a terminal state. The brief
  // delay lets the user see the completion state before the dock disappears.
  useEffect(() => {
    if (total === 0 || running > 0 || dismissed) return;
    const timer = setTimeout(() => {
      dismissAndClear();
    }, AUTO_DISMISS_DELAY_MS);
    return () => clearTimeout(timer);
  }, [total, running, dismissed, dismissAndClear]);

  const visible = total > 0 && !dismissed;

  if (!visible || typeof window === 'undefined') return null;

  return createPortal(
    <div className="dock">
      <div className="dock-header">
        <span className="dock-title">
          {running > 0 ? `${running} active` : `${total} complete`} · {total} total
        </span>
        <button className="dock-close" onClick={dismissAndClear} title="Dismiss">
          <CloseOutlined style={{ fontSize: 11 }} />
        </button>
      </div>

      <div className="dock-body">
        {dockTasks.map((task) => (
          <DockTaskRow key={task.id} task={task} />
        ))}
      </div>
    </div>,
    document.body
  );
}

// ── Individual task row ───────────────────────────────────────────────────────

function DockTaskRow({ task }: { task: DockTask }) {
  const removeUpload = useUploadStore((s) => s.removeTask);
  const removeDownload = useDownloadStore((s) => s.removeTask);

  const taskClass = `dock-task${task.state === 'done' ? ' done' : task.state === 'error' ? ' error' : ''}`;

  const icon =
    task.kind === 'upload' ? (
      <UploadOutlined />
    ) : task.kind === 'download' ? (
      <DownloadOutlined />
    ) : (
      <SwapOutlined />
    );

  const stateIcon =
    task.state === 'done' ? (
      <CheckOutlined style={{ color: '#4caf50', fontSize: 11 }} />
    ) : task.state === 'error' ? (
      <CloseCircleOutlined style={{ color: '#d4493a', fontSize: 11 }} />
    ) : null;

  const canCancel = task.state === 'active' && task.kind !== 'move';

  function handleCancel() {
    if (task.kind === 'upload') removeUpload(task.id);
    else if (task.kind === 'download') removeDownload(task.id);
  }

  return (
    <div className={taskClass}>
      <div className="dock-task-row">
        <span className="dock-task-icon">{icon}</span>
        <span className="dock-task-name">{task.name}</span>
        <span className="dock-task-pct">{stateIcon ?? `${Math.round(task.progress)}%`}</span>
        {canCancel && (
          <button className="dock-act" onClick={handleCancel} title="Cancel">
            <CloseOutlined style={{ fontSize: 10 }} />
          </button>
        )}
      </div>
      <div className="dock-task-bar">
        <div
          className="dock-task-fill"
          style={{ width: `${Math.max(0, Math.min(100, task.progress))}%` }}
        />
      </div>
    </div>
  );
}
