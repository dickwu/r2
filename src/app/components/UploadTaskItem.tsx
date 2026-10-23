'use client';

import { Button, Progress, Typography } from 'antd';
import {
  CheckCircleOutlined,
  ClockCircleOutlined,
  CloseCircleOutlined,
  LoadingOutlined,
  StopOutlined,
} from '@ant-design/icons';
import { showUploadFailure } from '@/app/lib/uploadRunner';
import { useUploadStore, type UploadTask } from '@/app/stores/uploadStore';

const { Text } = Typography;

interface UploadTaskItemProps {
  task: UploadTask;
}

// One row of the upload list. Starting, progress and retries are driven by the
// upload queue (uploadStore + uploadRunner); the row only shows them.
export default function UploadTaskItem({ task }: UploadTaskItemProps) {
  const removeTask = useUploadStore((s) => s.removeTask);
  const cancelTask = useUploadStore((s) => s.cancelTask);

  function handleCancel() {
    cancelTask(task.id);
  }

  function handleRemove() {
    removeTask(task.id);
  }

  const actions =
    task.status === 'pending'
      ? [
          <Button key="remove" type="text" size="small" danger onClick={handleRemove}>
            Remove
          </Button>,
        ]
      : task.status === 'uploading' || task.status === 'queued'
        ? [
            <Button
              key="cancel"
              type="text"
              size="small"
              danger
              icon={<StopOutlined />}
              onClick={handleCancel}
            ></Button>,
          ]
        : undefined;

  return (
    <div style={{ display: 'flex', alignItems: 'flex-start', padding: '8px 0', gap: 12 }}>
      <StatusIcon status={task.status} />
      <div style={{ flex: 1, minWidth: 0 }}>
        <Text ellipsis style={{ maxWidth: 280, display: 'block' }}>
          {task.fileName}
        </Text>
        {task.renamedFileName && task.status === 'pending' && (
          <Text type="secondary" style={{ fontSize: 11, display: 'block' }}>
            {'→ '}
            {task.renamedFileName}
          </Text>
        )}
        <TaskDescription task={task} />
      </div>
      {actions && <div style={{ display: 'flex', gap: 4 }}>{actions}</div>}
    </div>
  );
}

function StatusIcon({ status }: { status: UploadTask['status'] }) {
  switch (status) {
    case 'success':
      return <CheckCircleOutlined style={{ color: '#52c41a', fontSize: 16 }} />;
    case 'error':
      return <CloseCircleOutlined style={{ color: '#ff4d4f', fontSize: 16 }} />;
    case 'cancelled':
      return <StopOutlined style={{ color: '#999', fontSize: 16 }} />;
    case 'uploading':
      return <LoadingOutlined style={{ color: '#1677ff', fontSize: 16 }} />;
    case 'queued':
      return <ClockCircleOutlined style={{ color: '#999', fontSize: 16 }} />;
    default:
      return null;
  }
}

function TaskDescription({ task }: { task: UploadTask }) {
  switch (task.status) {
    case 'uploading': {
      const uploadedBytes = (task.progress / 100) * task.fileSize;
      const remainingBytes = task.fileSize - uploadedBytes;
      const eta = task.speed > 0 ? remainingBytes / task.speed : 0;

      return (
        <div>
          <Progress percent={task.progress} size="small" />
          <Text type="secondary" style={{ fontSize: 11 }}>
            {task.retry ? (
              <span title={task.retry.error}>
                Retrying ({task.retry.attempt}/{task.retry.maxAttempts})…
              </span>
            ) : task.speed > 0 ? (
              <>
                {formatSpeed(task.speed)}
                <span style={{ marginLeft: 8, color: '#999' }}>{formatTimeLeft(eta)}</span>
              </>
            ) : (
              'Initializing...'
            )}
          </Text>
        </div>
      );
    }
    case 'queued':
      return (
        <Text type="secondary" style={{ fontSize: 12 }}>
          Queued · {formatFileSize(task.fileSize)}
        </Text>
      );
    case 'error':
      return (
        <span className="task-failed">
          Failed
          <button
            type="button"
            className="btn btn-sm btn-danger-ghost"
            onClick={() => showUploadFailure(task)}
          >
            Show error
          </button>
        </span>
      );
    case 'success':
      return (
        <Text type="success" style={{ fontSize: 12 }}>
          Uploaded
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
