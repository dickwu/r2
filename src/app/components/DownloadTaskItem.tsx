'use client';

import { memo, useState } from 'react';
import { Button, Progress, Typography, Space, Tooltip } from 'antd';
import {
  CheckCircleOutlined,
  CloseCircleOutlined,
  LoadingOutlined,
  PauseCircleOutlined,
  PlayCircleOutlined,
  DeleteOutlined,
  StopOutlined,
  DownOutlined,
  RightOutlined,
  FolderOpenOutlined,
} from '@ant-design/icons';
import { invoke } from '@tauri-apps/api/core';
import { type DownloadTask, type DownloadChunk } from '@/app/stores/downloadStore';

const { Text } = Typography;

interface DownloadTaskItemProps {
  task: DownloadTask;
  onResume?: () => void;
}

function DownloadTaskItem({ task, onResume }: DownloadTaskItemProps) {
  const [expanded, setExpanded] = useState(false);

  async function handlePause() {
    try {
      await invoke('pause_download', { taskId: task.id });
    } catch (e) {
      console.error('Failed to pause download:', e);
    }
  }

  async function handleCancel() {
    try {
      await invoke('cancel_download', { taskId: task.id });
    } catch (e) {
      console.error('Failed to cancel download:', e);
    }
  }

  async function handleDelete() {
    try {
      await invoke('delete_download_task', { taskId: task.id });
    } catch (e) {
      console.error('Failed to delete download task:', e);
    }
  }

  function handleResume() {
    if (onResume) onResume();
  }

  async function handleShowInFolder() {
    try {
      // Use tauri-plugin-opener to reveal the file in Finder/Explorer
      const { revealItemInDir } = await import('@tauri-apps/plugin-opener');
      const filePath = `${task.localPath}/${task.fileName}`;
      await revealItemInDir(filePath);
    } catch {
      // Fallback: try opening the folder itself
      try {
        const { openPath } = await import('@tauri-apps/plugin-opener');
        await openPath(task.localPath);
      } catch (e) {
        console.error('Failed to open folder:', e);
      }
    }
  }

  const hasChunks = task.chunks.length > 1;
  const canExpand = hasChunks && (task.status === 'downloading' || task.status === 'paused');

  const getActions = () => {
    switch (task.status) {
      case 'pending':
        return (
          <Space size={4}>
            <Tooltip title="Delete">
              <Button
                type="text"
                size="small"
                danger
                icon={<DeleteOutlined />}
                onClick={handleDelete}
              />
            </Tooltip>
          </Space>
        );
      case 'downloading':
        return (
          <Space size={4}>
            <Tooltip title="Pause">
              <Button
                type="text"
                size="small"
                icon={<PauseCircleOutlined />}
                onClick={handlePause}
              />
            </Tooltip>
            <Tooltip title="Cancel">
              <Button
                type="text"
                size="small"
                danger
                icon={<StopOutlined />}
                onClick={handleCancel}
              />
            </Tooltip>
          </Space>
        );
      case 'paused':
        return (
          <Space size={4}>
            <Tooltip title="Resume">
              <Button
                type="text"
                size="small"
                icon={<PlayCircleOutlined />}
                onClick={handleResume}
                style={{ color: 'var(--color-success)' }}
              />
            </Tooltip>
            <Tooltip title="Cancel">
              <Button
                type="text"
                size="small"
                danger
                icon={<StopOutlined />}
                onClick={handleCancel}
              />
            </Tooltip>
            <Tooltip title="Delete">
              <Button
                type="text"
                size="small"
                danger
                icon={<DeleteOutlined />}
                onClick={handleDelete}
              />
            </Tooltip>
          </Space>
        );
      case 'error':
        return (
          <Space size={4}>
            <Tooltip title="Retry">
              <Button
                type="text"
                size="small"
                icon={<PlayCircleOutlined />}
                onClick={handleResume}
                style={{ color: 'var(--color-link)' }}
              />
            </Tooltip>
            <Tooltip title="Delete">
              <Button
                type="text"
                size="small"
                danger
                icon={<DeleteOutlined />}
                onClick={handleDelete}
              />
            </Tooltip>
          </Space>
        );
      case 'success':
        return (
          <Space size={4}>
            <Tooltip title="Show in Folder">
              <Button
                type="text"
                size="small"
                icon={<FolderOpenOutlined />}
                onClick={handleShowInFolder}
              />
            </Tooltip>
            <Tooltip title="Delete">
              <Button
                type="text"
                size="small"
                danger
                icon={<DeleteOutlined />}
                onClick={handleDelete}
              />
            </Tooltip>
          </Space>
        );
      case 'cancelled':
        return (
          <Tooltip title="Delete">
            <Button
              type="text"
              size="small"
              danger
              icon={<DeleteOutlined />}
              onClick={handleDelete}
            />
          </Tooltip>
        );
      default:
        return null;
    }
  };

  return (
    <div style={{ padding: '8px 0' }}>
      <div style={{ display: 'flex', alignItems: 'flex-start', gap: 12 }}>
        <StatusIcon status={task.status} />
        <div style={{ flex: 1, minWidth: 0 }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 4 }}>
            {canExpand && (
              <span
                onClick={() => setExpanded(!expanded)}
                style={{ cursor: 'pointer', fontSize: 10, color: 'var(--color-text-secondary)' }}
                aria-expanded={expanded}
                aria-controls={`chunk-detail-${task.id}`}
              >
                {expanded ? <DownOutlined /> : <RightOutlined />}
              </span>
            )}
            <Text ellipsis style={{ maxWidth: 320, display: 'block' }}>
              {task.fileName}
            </Text>
          </div>
          <TaskDescription task={task} />
        </div>
        <div style={{ display: 'flex', gap: 4 }}>{getActions()}</div>
      </div>

      {/* Expandable chunk detail view */}
      {expanded && canExpand && (
        <div
          id={`chunk-detail-${task.id}`}
          style={{
            marginTop: 8,
            marginLeft: 28,
            padding: '8px 12px',
            borderRadius: 6,
            backgroundColor: 'var(--color-bg-surface)',
            border: '1px solid var(--color-border-subtle)',
            transition: 'max-height 0.2s ease-out',
          }}
        >
          {task.chunks.map((chunk) => (
            <ChunkRow key={chunk.chunkId} chunk={chunk} fileSize={task.fileSize} />
          ))}
          {task.speedHistory.length > 1 && (
            <div style={{ marginTop: 8 }}>
              <Text type="secondary" style={{ fontSize: 11 }}>
                Speed history
              </Text>
              <SparklineLarge data={task.speedHistory} />
            </div>
          )}
          {task.peakSpeed > 0 && (
            <Text type="secondary" style={{ fontSize: 11, display: 'block', marginTop: 4 }}>
              Peak: {formatSpeed(task.peakSpeed)}
            </Text>
          )}
        </div>
      )}
    </div>
  );
}

function ChunkRow({ chunk, fileSize }: { chunk: DownloadChunk; fileSize: number }) {
  const totalBytes = chunk.endByte - chunk.startByte;
  const pct = totalBytes > 0 ? Math.round((chunk.downloadedBytes / totalBytes) * 100) : 0;
  const rangeLabel = `${formatFileSize(chunk.startByte)}–${formatFileSize(chunk.endByte)}`;

  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 4 }}>
      <Text type="secondary" style={{ fontSize: 11, width: 50, flexShrink: 0 }}>
        Chunk {chunk.chunkId + 1}
      </Text>
      <div style={{ flex: 1 }}>
        <Progress
          percent={pct}
          size={['100%', 6]}
          showInfo={false}
          strokeColor={chunk.status === 'complete' ? 'var(--color-success)' : 'var(--color-link)'}
          railColor="var(--color-border-control)"
        />
      </div>
      <Text type="secondary" style={{ fontSize: 11, width: 40, textAlign: 'right', flexShrink: 0 }}>
        {pct}%
      </Text>
      {chunk.speed > 0 && (
        <Text
          type="secondary"
          style={{ fontSize: 11, width: 65, textAlign: 'right', flexShrink: 0 }}
        >
          {formatSpeed(chunk.speed)}
        </Text>
      )}
      {chunk.status === 'complete' && (
        <CheckCircleOutlined style={{ color: 'var(--color-success)', fontSize: 12 }} />
      )}
    </div>
  );
}

/** Larger sparkline for expanded detail view */
function SparklineLarge({ data }: { data: number[] }) {
  if (data.length < 2) return null;
  const width = 120;
  const height = 24;
  const max = Math.max(...data, 1);
  const points = data
    .map((v, i) => {
      const x = (i / (data.length - 1)) * width;
      const y = height - (v / max) * height;
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(' ');

  return (
    <svg
      width={width}
      height={height}
      viewBox={`0 0 ${width} ${height}`}
      style={{ display: 'block' }}
    >
      <polyline
        points={points}
        fill="none"
        stroke="var(--color-link)"
        strokeWidth="1.5"
        strokeLinejoin="round"
        strokeLinecap="round"
      />
    </svg>
  );
}

function StatusIcon({ status }: { status: DownloadTask['status'] }) {
  switch (status) {
    case 'success':
      return <CheckCircleOutlined style={{ color: 'var(--color-success)', fontSize: 16 }} />;
    case 'error':
      return <CloseCircleOutlined style={{ color: 'var(--color-error, #ff4d4f)', fontSize: 16 }} />;
    case 'cancelled':
      return <StopOutlined style={{ color: 'var(--color-text-tertiary)', fontSize: 16 }} />;
    case 'downloading':
      return <LoadingOutlined style={{ color: 'var(--color-link)', fontSize: 16 }} />;
    case 'paused':
      return (
        <PauseCircleOutlined style={{ color: 'var(--color-warning, #faad14)', fontSize: 16 }} />
      );
    default:
      return null;
  }
}

function TaskDescription({ task }: { task: DownloadTask }) {
  switch (task.status) {
    case 'downloading': {
      const remainingBytes = Math.max(0, task.fileSize - task.downloadedBytes);
      const eta = task.speed > 0 ? remainingBytes / task.speed : 0;
      const chunkLabel = task.chunkCount > 1 ? ` · ${task.chunkCount} chunks` : '';
      // Show "finishing..." when > 97% — the last few percent often take longer
      // due to file assembly, rename, and integrity checks
      const isFinishing = task.progress >= 97;

      return (
        <div>
          <Progress
            percent={task.progress}
            status="active"
            strokeColor={{ from: 'var(--color-link)', to: 'var(--color-success)' }}
            railColor="var(--color-border-control)"
            size={['100%', 8]}
          />
          <Text type="secondary" style={{ fontSize: 11 }}>
            {task.speed > 0 ? (
              <>
                {formatSpeed(task.speed)}
                {chunkLabel}
                <span style={{ marginLeft: 8, color: 'var(--color-text-tertiary)' }}>
                  {isFinishing ? 'finishing...' : formatTimeLeft(eta)}
                </span>
              </>
            ) : task.downloadedBytes > 0 ? (
              `${formatFileSize(task.downloadedBytes)} / ${formatFileSize(task.fileSize)}`
            ) : (
              'Connecting...'
            )}
          </Text>
        </div>
      );
    }
    case 'paused': {
      return (
        <div>
          <Progress
            percent={Math.round(task.progress)}
            status="exception"
            strokeColor="var(--color-warning, #faad14)"
            railColor="var(--color-border-control)"
            size={['100%', 8]}
          />
          <Text style={{ fontSize: 12, color: 'var(--color-warning, #faad14)' }}>
            Paused - {formatFileSize(task.downloadedBytes)} / {formatFileSize(task.fileSize)}
          </Text>
        </div>
      );
    }
    case 'error':
      return (
        <Text style={{ fontSize: 12, color: 'var(--color-error, #ff4d4f)' }}>
          {task.error || 'Download failed'}
        </Text>
      );
    case 'success':
      return (
        <Text style={{ fontSize: 12, color: 'var(--color-success)' }}>
          Downloaded · {formatFileSize(task.fileSize)}
        </Text>
      );
    case 'cancelled':
      return (
        <Text type="secondary" style={{ fontSize: 12 }}>
          Cancelled
        </Text>
      );
    default:
      return (
        <Text type="secondary" style={{ fontSize: 12 }}>
          {formatFileSize(task.fileSize)}
        </Text>
      );
  }
}

function formatFileSize(bytes: number): string {
  if (bytes === 0) return '0 B';
  const k = 1024;
  const sizes = ['B', 'KB', 'MB', 'GB'];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + ' ' + sizes[i];
}

function formatSpeed(bytesPerSecond: number): string {
  if (bytesPerSecond === 0) return '0 B/s';
  const k = 1024;
  const sizes = ['B/s', 'KB/s', 'MB/s', 'GB/s'];
  const i = Math.floor(Math.log(bytesPerSecond) / Math.log(k));
  return parseFloat((bytesPerSecond / Math.pow(k, i)).toFixed(1)) + ' ' + sizes[i];
}

function formatTimeLeft(seconds: number): string {
  if (seconds <= 0) return '';
  if (seconds < 60) return `${Math.ceil(seconds)}s left`;
  if (seconds < 3600) {
    const mins = Math.floor(seconds / 60);
    const secs = Math.ceil(seconds % 60);
    return secs > 0 ? `${mins}m ${secs}s left` : `${mins}m left`;
  }
  const hours = Math.floor(seconds / 3600);
  const mins = Math.floor((seconds % 3600) / 60);
  return mins > 0 ? `${hours}h ${mins}m left` : `${hours}h left`;
}

export default memo(DownloadTaskItem, (prevProps, nextProps) => {
  const prev = prevProps.task;
  const next = nextProps.task;
  return (
    prev.id === next.id &&
    prev.status === next.status &&
    prev.progress === next.progress &&
    prev.speed === next.speed &&
    prev.downloadedBytes === next.downloadedBytes &&
    prev.error === next.error &&
    prev.chunkCount === next.chunkCount &&
    prev.chunks.length === next.chunks.length &&
    prev.peakSpeed === next.peakSpeed
  );
});
