'use client';

import React, { useState, useEffect, useMemo, useCallback } from 'react';
import { createPortal } from 'react-dom';
import { invoke } from '@tauri-apps/api/core';
import {
  UploadOutlined,
  DownloadOutlined,
  SwapOutlined,
  EditOutlined,
  CheckOutlined,
  CloseOutlined,
  CloseCircleOutlined,
  PauseOutlined,
  CaretRightOutlined,
  ShrinkOutlined,
} from '@ant-design/icons';
import { useUploadStore } from '@/app/stores/uploadStore';
import { useDownloadStore } from '@/app/stores/downloadStore';
import { useMoveStore } from '@/app/stores/moveStore';
import { useRenameStore } from '@/app/stores/renameStore';
import { formatBytes, formatSpeed, formatEta } from '@/app/utils/formatBytes';
import { etaSeconds } from '@/app/lib/progressThrottle';

// How long to keep the dock visible after the last task completes,
// so the user briefly sees the success state before it auto-dismisses.
const AUTO_DISMISS_DELAY_MS = 2500;

// ── Unified task shape ────────────────────────────────────────────────────────

type TaskKind = 'upload' | 'download' | 'move' | 'rename';
type TaskState = 'active' | 'paused' | 'done' | 'error';

interface DockTask {
  id: string;
  kind: TaskKind;
  name: string;
  /** Micro phase chip, e.g. move sub-phase. */
  phase?: string;
  progress: number;
  state: TaskState;
  speed: number; // bytes/sec (0 when n/a)
  transferred: number; // bytes
  total: number; // bytes (0 when unknown)
  /** Count-based progress for rename batches. */
  countDone?: number;
  countTotal?: number;
  opsPerSec?: number;
  etaSec: number;
  canPause: boolean;
  canResume: boolean;
  canCancel: boolean;
}

function uploadState(status: string): TaskState {
  if (status === 'success') return 'done';
  if (status === 'error' || status === 'cancelled') return 'error';
  return 'active';
}

function downloadState(status: string): TaskState {
  if (status === 'success') return 'done';
  if (status === 'error' || status === 'cancelled') return 'error';
  if (status === 'paused') return 'paused';
  return 'active';
}

function moveState(status: string): TaskState {
  if (status === 'success') return 'done';
  if (status === 'error' || status === 'cancelled') return 'error';
  if (status === 'paused') return 'paused';
  return 'active';
}

const MOVE_PHASE_LABEL: Record<string, string> = {
  downloading: 'DOWN',
  uploading: 'UP',
  finishing: 'FINISH',
  deleting: 'CLEAN',
};

// ── Component ─────────────────────────────────────────────────────────────────

export default function TransferDock() {
  const uploadTasks = useUploadStore((s) => s.tasks);
  const downloadTasks = useDownloadStore((s) => s.tasks);
  const moveTasks = useMoveStore((s) => s.tasks);
  const renameBatches = useRenameStore((s) => s.batches);

  const clearFinishedUploads = useUploadStore((s) => s.clearFinished);
  const clearFinishedDownloads = useDownloadStore((s) => s.clearFinished);
  const clearFinishedMoves = useMoveStore((s) => s.clearFinishedTasks);
  const clearFinishedRenames = useRenameStore((s) => s.clearFinished);

  // "dismissed" resets when a NEW task arrives
  const [dismissed, setDismissed] = useState(false);
  const [collapsed, setCollapsed] = useState(false);
  const [prevTotal, setPrevTotal] = useState(0);

  const dockTasks = useMemo<DockTask[]>(() => {
    const out: DockTask[] = [];

    for (const t of uploadTasks) {
      const state = uploadState(t.status);
      out.push({
        id: t.id,
        kind: 'upload',
        name: t.renamedFileName ?? t.fileName,
        progress: t.progress,
        state,
        speed: state === 'active' ? t.speed : 0,
        transferred: Math.round((t.progress / 100) * t.fileSize),
        total: t.fileSize,
        etaSec:
          state === 'active' ? etaSeconds(t.fileSize, (t.progress / 100) * t.fileSize, t.speed) : 0,
        canPause: false,
        canResume: false,
        canCancel: state === 'active',
      });
    }
    for (const t of downloadTasks) {
      const state = downloadState(t.status);
      out.push({
        id: t.id,
        kind: 'download',
        name: t.fileName,
        progress: t.progress,
        state,
        speed: t.status === 'downloading' ? t.speed : 0,
        transferred: t.downloadedBytes,
        total: t.fileSize,
        etaSec: t.status === 'downloading' ? etaSeconds(t.fileSize, t.downloadedBytes, t.speed) : 0,
        canPause: t.status === 'downloading',
        canResume: t.status === 'paused',
        canCancel: t.status === 'downloading' || t.status === 'paused' || t.status === 'pending',
      });
    }
    for (const t of moveTasks) {
      const state = moveState(t.status);
      const name = t.sourceKey.split('/').filter(Boolean).pop() ?? t.sourceKey;
      const transferring = t.status === 'downloading' || t.status === 'uploading';
      out.push({
        id: t.id,
        kind: 'move',
        name,
        phase: state === 'active' ? MOVE_PHASE_LABEL[t.phase] : undefined,
        progress: t.progress,
        state,
        speed: transferring ? t.speed : 0,
        transferred: t.transferredBytes,
        total: t.fileSize,
        etaSec: transferring ? etaSeconds(t.fileSize, t.transferredBytes, t.speed) : 0,
        canPause: transferring,
        canResume: t.status === 'paused',
        canCancel: transferring || t.status === 'paused' || t.status === 'pending',
      });
    }
    for (const b of renameBatches) {
      const state: TaskState =
        b.status === 'running' ? 'active' : b.status === 'success' ? 'done' : 'error';
      out.push({
        id: b.id,
        kind: 'rename',
        name: b.label,
        phase: state === 'active' ? 'RENAME' : undefined,
        progress: b.total > 0 ? (b.completed / b.total) * 100 : 0,
        state,
        speed: 0,
        transferred: 0,
        total: 0,
        countDone: b.completed,
        countTotal: b.total,
        opsPerSec: b.opsPerSec,
        etaSec: state === 'active' ? b.etaMs / 1000 : 0,
        canPause: false,
        canResume: false,
        canCancel: false,
      });
    }

    return out;
  }, [uploadTasks, downloadTasks, moveTasks, renameBatches]);

  const total = dockTasks.length;
  const running = dockTasks.filter((t) => t.state === 'active').length;
  const paused = dockTasks.filter((t) => t.state === 'paused').length;
  const failed = dockTasks.filter((t) => t.state === 'error').length;

  // Aggregate progress + combined throughput across all lanes. Transfers are
  // byte-weighted; rename batches are count-based. The two domains blend
  // weighted by how many dock rows each contributes.
  const agg = useMemo(() => {
    let totalBytes = 0;
    let doneBytes = 0;
    let speed = 0;
    let countDone = 0;
    let countTotal = 0;
    let byteTaskCount = 0;
    let renameCount = 0;

    for (const t of dockTasks) {
      if (t.kind === 'rename') {
        countDone += t.countDone ?? 0;
        countTotal += t.countTotal ?? 0;
        renameCount += 1;
        continue;
      }
      if (t.total > 0) {
        totalBytes += t.total;
        doneBytes += Math.min(t.transferred, t.total);
        byteTaskCount += 1;
      }
      if (t.state === 'active') speed += t.speed;
    }

    const bytePct = totalBytes > 0 ? (doneBytes / totalBytes) * 100 : 0;
    const countPct = countTotal > 0 ? (countDone / countTotal) * 100 : 0;
    const weightSum = byteTaskCount + renameCount;
    const percent =
      weightSum > 0 ? (bytePct * byteTaskCount + countPct * renameCount) / weightSum : 0;

    return {
      percent: Math.max(0, Math.min(100, percent)),
      speed,
      totalBytes,
      doneBytes,
      etaSec: speed > 0 ? Math.max(0, (totalBytes - doneBytes) / speed) : 0,
    };
  }, [dockTasks]);

  // Centralised dismiss path: hide dock + flush finished tasks from stores so
  // they don't reappear alongside the next new task.
  const dismissAndClear = useCallback(() => {
    setDismissed(true);
    clearFinishedUploads();
    clearFinishedDownloads();
    clearFinishedMoves();
    clearFinishedRenames();
  }, [clearFinishedUploads, clearFinishedDownloads, clearFinishedMoves, clearFinishedRenames]);

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
    if (total === 0 || running > 0 || paused > 0 || dismissed) return;
    const timer = setTimeout(() => {
      dismissAndClear();
    }, AUTO_DISMISS_DELAY_MS);
    return () => clearTimeout(timer);
  }, [total, running, paused, dismissed, dismissAndClear]);

  const visible = total > 0 && !dismissed;

  if (!visible || typeof window === 'undefined') return null;

  if (collapsed) {
    return createPortal(
      <button className="dock-pill" onClick={() => setCollapsed(false)} title="Expand transfers">
        <ProgressRing percent={agg.percent} active={running > 0} />
        <span className="dock-pill-count">
          {running > 0 ? running : <CheckOutlined style={{ fontSize: 11 }} />}
        </span>
        {agg.speed > 0 && <span className="dock-pill-speed">{formatSpeed(agg.speed)}</span>}
      </button>,
      document.body
    );
  }

  const summary =
    running > 0
      ? `${running} active`
      : paused > 0
        ? `${paused} paused`
        : failed > 0
          ? `${total - failed} done · ${failed} failed`
          : `${total} complete`;

  return createPortal(
    <div className="dock">
      <div className="dock-header">
        <ProgressRing percent={agg.percent} active={running > 0} />
        <span className="dock-title">{summary}</span>
        <span className="dock-header-total">{total} total</span>
        <button className="dock-close" onClick={() => setCollapsed(true)} title="Collapse">
          <ShrinkOutlined style={{ fontSize: 11 }} />
        </button>
        <button className="dock-close" onClick={dismissAndClear} title="Dismiss">
          <CloseOutlined style={{ fontSize: 11 }} />
        </button>
      </div>

      <div className="dock-overall">
        <div
          className={`dock-overall-fill${running > 0 ? 'active' : ''}`}
          style={{ width: `${agg.percent}%` }}
        />
      </div>

      <div className="dock-telemetry">
        <span className="dock-telemetry-cell">
          <span className="dock-telemetry-label">RATE</span>
          {formatSpeed(agg.speed)}
        </span>
        {agg.totalBytes > 0 && (
          <span className="dock-telemetry-cell">
            <span className="dock-telemetry-label">DATA</span>
            {formatBytes(agg.doneBytes)} / {formatBytes(agg.totalBytes)}
          </span>
        )}
        <span className="dock-telemetry-cell">
          <span className="dock-telemetry-label">ETA</span>
          {agg.etaSec > 0 ? formatEta(agg.etaSec).replace(' left', '') : '—'}
        </span>
      </div>

      <div className="dock-body">
        {dockTasks.map((task) => (
          <DockTaskRow key={`${task.kind}:${task.id}`} task={task} />
        ))}
      </div>
    </div>,
    document.body
  );
}

// ── Aggregate progress ring ───────────────────────────────────────────────────

function ProgressRing({ percent, active }: { percent: number; active: boolean }) {
  const r = 7;
  const c = 2 * Math.PI * r;
  const filled = (Math.max(0, Math.min(100, percent)) / 100) * c;
  return (
    <svg className={`dock-ring${active ? 'active' : ''}`} viewBox="0 0 20 20" aria-hidden>
      <circle className="dock-ring-track" cx="10" cy="10" r={r} />
      <circle
        className="dock-ring-fill"
        cx="10"
        cy="10"
        r={r}
        strokeDasharray={`${filled} ${c - filled}`}
        strokeDashoffset={c / 4}
      />
    </svg>
  );
}

// ── Individual task row ───────────────────────────────────────────────────────

const KIND_ICON: Record<TaskKind, React.ReactNode> = {
  upload: <UploadOutlined />,
  download: <DownloadOutlined />,
  move: <SwapOutlined />,
  rename: <EditOutlined />,
};

function DockTaskRow({ task }: { task: DockTask }) {
  const removeUpload = useUploadStore((s) => s.removeTask);

  const stateClass =
    task.state === 'done'
      ? ' done'
      : task.state === 'error'
        ? ' error'
        : task.state === 'paused'
          ? ' paused'
          : '';

  const stateIcon =
    task.state === 'done' ? (
      <CheckOutlined className="dock-state-ok" />
    ) : task.state === 'error' ? (
      <CloseCircleOutlined className="dock-state-err" />
    ) : null;

  async function handlePause() {
    try {
      if (task.kind === 'download') await invoke('pause_download', { taskId: task.id });
      else if (task.kind === 'move') await invoke('pause_move', { taskId: task.id });
    } catch (e) {
      console.error('Failed to pause task:', e);
    }
  }

  async function handleResume() {
    try {
      if (task.kind === 'download') await invoke('resume_download', { taskId: task.id });
      else if (task.kind === 'move') await invoke('resume_move', { taskId: task.id });
    } catch (e) {
      console.error('Failed to resume task:', e);
    }
  }

  async function handleCancel() {
    try {
      if (task.kind === 'upload') removeUpload(task.id);
      else if (task.kind === 'download') await invoke('cancel_download', { taskId: task.id });
      else if (task.kind === 'move') await invoke('cancel_move', { taskId: task.id });
    } catch (e) {
      console.error('Failed to cancel task:', e);
    }
  }

  // Stats line: bytes for transfers, counts for rename batches
  const stats: string[] = [];
  if (task.kind === 'rename') {
    if (task.countTotal) stats.push(`${task.countDone ?? 0}/${task.countTotal} files`);
    if (task.state === 'active' && (task.opsPerSec ?? 0) > 0.1)
      stats.push(`${(task.opsPerSec ?? 0).toFixed(1)} files/s`);
  } else {
    if (task.total > 0)
      stats.push(
        `${formatBytes(Math.min(task.transferred, task.total))} / ${formatBytes(task.total)}`
      );
    if (task.state === 'active' && task.speed > 0) stats.push(formatSpeed(task.speed));
  }
  if (task.state === 'active' && task.etaSec > 0) stats.push(formatEta(task.etaSec));
  if (task.state === 'paused') stats.push('paused');

  return (
    <div className={`dock-task${stateClass}`} data-kind={task.kind}>
      <div className="dock-task-row">
        <span className="dock-task-icon">{KIND_ICON[task.kind]}</span>
        <span className="dock-task-name" title={task.name}>
          {task.name}
        </span>
        {task.phase && <span className="dock-chip">{task.phase}</span>}
        <span className="dock-task-pct">{stateIcon ?? `${Math.round(task.progress)}%`}</span>
        <span className="dock-actions">
          {task.canPause && (
            <button className="dock-act" onClick={handlePause} title="Pause">
              <PauseOutlined style={{ fontSize: 10 }} />
            </button>
          )}
          {task.canResume && (
            <button className="dock-act" onClick={handleResume} title="Resume">
              <CaretRightOutlined style={{ fontSize: 10 }} />
            </button>
          )}
          {task.canCancel && (
            <button className="dock-act" onClick={handleCancel} title="Cancel">
              <CloseOutlined style={{ fontSize: 10 }} />
            </button>
          )}
        </span>
      </div>
      <div className="dock-task-bar">
        <div
          className={`dock-task-fill${task.state === 'active' ? 'active' : ''}`}
          style={{ width: `${Math.max(0, Math.min(100, task.progress))}%` }}
        />
      </div>
      {stats.length > 0 && <div className="dock-task-stats">{stats.join(' · ')}</div>}
    </div>
  );
}
