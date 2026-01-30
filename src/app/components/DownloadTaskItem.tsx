'use client';

import { memo } from 'react';
import { Button, Progress, Typography, Space, Tooltip } from 'antd';
import {
  CheckCircleOutlined,
  CloseCircleOutlined,
  LoadingOutlined,
  PauseCircleOutlined,
  PlayCircleOutlined,
  DeleteOutlined,
  StopOutlined,
} from '@ant-design/icons';
import { invoke } from '@tauri-apps/api/core';
import { type DownloadTask } from '@/app/stores/downloadStore';

const { Text } = Typography;

interface DownloadTaskItemProps {
  task: DownloadTask;
  onResume?: () => void;
}

function DownloadTaskItem({ task, onResume }: DownloadTaskItemProps) {
  // Pause download - UI will update via download-status-changed event
  async function handlePause() {
    try {
      await invoke('pause_download', { taskId: task.id });
    } catch (e) {
      console.error('Failed to pause download:', e);
    }
  }

  // Cancel download - UI will update via download-status-changed event
  async function handleCancel() {
    try {
      await invoke('cancel_download', { taskId: task.id });
    } catch (e) {
      console.error('Failed to cancel download:', e);
    }
  }

  // Delete task - UI will update via download-task-deleted event
  async function handleDelete() {
    try {
      await invoke('delete_download_task', { taskId: task.id });
    } catch (e) {
      console.error('Failed to delete download task:', e);
    }
  }

  function handleResume() {
    if (onResume) {
      onResume();
    }
  }

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
                style={{ color: '#52c41a' }}
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
                style={{ color: '#1677ff' }}
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
    <div style={{ display: 'flex', alignItems: 'flex-start', padding: '8px 0', gap: 12 }}>
      <StatusIcon status={task.status} />
      <div style={{ flex: 1, minWidth: 0 }}>
        <Text ellipsis style={{ maxWidth: 280, display: 'block' }}>
          {task.fileName}
        </Text>
        <TaskDescription task={task} />
      </div>
      <div style={{ display: 'flex', gap: 4 }}>{getActions()}</div>
    </div>
  );
}

function StatusIcon({ status }: { status: DownloadTask['status'] }) {
  switch (status) {
    case 'success':
      return <CheckCircleOutlined style={{ color: '#52c41a', fontSize: 16 }} />;
    case 'error':
      return <CloseCircleOutlined style={{ color: '#ff4d4f', fontSize: 16 }} />;
    case 'cancelled':
      return <StopOutlined style={{ color: '#999', fontSize: 16 }} />;
    case 'downloading':
      return <LoadingOutlined style={{ color: '#1677ff', fontSize: 16 }} />;
    case 'paused':
      return <PauseCircleOutlined style={{ color: '#faad14', fontSize: 16 }} />;
    default:
      return null;
  }
}

function TaskDescription({ task }: { task: DownloadTask }) {
  switch (task.status) {
    case 'downloading': {
      const remainingBytes = task.fileSize - task.downloadedBytes;
      const eta = task.speed > 0 ? remainingBytes / task.speed : 0;

      return (
        <div>
          <Progress
            percent={task.progress}
            status="active"
            strokeColor={{ from: '#108ee9', to: '#87d068' }}
            railColor="#e8e8e8"
            size={['100%', 8]}
          />
          <Text type="secondary" style={{ fontSize: 11 }}>
            {task.speed > 0 ? (
              <>
                {formatSpeed(task.speed)}
                <span style={{ marginLeft: 8, color: '#999' }}>{formatTimeLeft(eta)}</span>
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
            strokeColor="#faad14"
            railColor="#e8e8e8"
            size={['100%', 8]}
          />
          <Text type="warning" style={{ fontSize: 12 }}>
            Paused - {formatFileSize(task.downloadedBytes)} / {formatFileSize(task.fileSize)}
          </Text>
        </div>
      );
    }
    case 'error':
      return (
        <Text type="danger" style={{ fontSize: 12 }}>
          {task.error || 'Download failed'}
        </Text>
      );
    case 'success':
      return (
        <Text type="success" style={{ fontSize: 12 }}>
          Downloaded
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

// Memoize to prevent unnecessary re-renders in the list
export default memo(DownloadTaskItem, (prevProps, nextProps) => {
  const prev = prevProps.task;
  const next = nextProps.task;
  // Only re-render if task data that affects display has changed
  return (
    prev.id === next.id &&
    prev.status === next.status &&
    prev.progress === next.progress &&
    prev.speed === next.speed &&
    prev.downloadedBytes === next.downloadedBytes &&
    prev.error === next.error
  );
});
