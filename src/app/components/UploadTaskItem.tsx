'use client';

import { useEffect, useRef } from 'react';
import { Button, Progress, Typography } from 'antd';
import {
  CheckCircleOutlined,
  CloseCircleOutlined,
  LoadingOutlined,
  StopOutlined,
} from '@ant-design/icons';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { useUploadStore, type UploadTask } from '@/app/stores/uploadStore';

const { Text } = Typography;

interface UploadProgress {
  task_id: string;
  percent: number;
  uploaded_bytes: number;
  total_bytes: number;
  speed: number;
}

interface UploadResult {
  task_id: string;
  success: boolean;
  error?: string;
}

interface UploadTaskItemProps {
  task: UploadTask;
}

export default function UploadTaskItem({ task }: UploadTaskItemProps) {
  const config = useUploadStore((s) => s.config);
  const uploadPath = useUploadStore((s) => s.uploadPath);
  const updateTask = useUploadStore((s) => s.updateTask);
  const removeTask = useUploadStore((s) => s.removeTask);

  const isUploadingRef = useRef(false);
  const unlistenRef = useRef<UnlistenFn | null>(null);

  // Start upload when status changes to 'uploading'
  useEffect(() => {
    if (task.status !== 'uploading' || isUploadingRef.current || !config) return;
    if (!config.accessKeyId || !config.secretAccessKey) return;
    if (config.provider === 'aws' && !config.region) return;
    if (
      (config.provider === 'minio' || config.provider === 'rustfs') &&
      (!config.endpointHost || !config.endpointScheme)
    ) {
      return;
    }

    isUploadingRef.current = true;

    const normalizedPath = uploadPath
      ? uploadPath.endsWith('/')
        ? uploadPath
        : uploadPath + '/'
      : '';
    const key = normalizedPath + task.fileName;

    // Listen for progress events
    listen<UploadProgress>('upload-progress', (event) => {
      if (event.payload.task_id === task.id) {
        updateTask(task.id, {
          progress: event.payload.percent,
          speed: event.payload.speed,
        });
      }
    }).then((unlisten) => {
      unlistenRef.current = unlisten;
    });

    // Call Rust upload function
    const command =
      config.provider === 'r2'
        ? 'upload_file'
        : config.provider === 'aws'
          ? 'upload_aws_file'
          : config.provider === 'minio'
            ? 'upload_minio_file'
            : 'upload_rustfs_file';

    const endpointScheme =
      config.provider === 'minio' || config.provider === 'rustfs'
        ? config.endpointScheme
        : config.provider === 'aws'
          ? (config.endpointScheme ?? undefined)
          : undefined;
    const endpointHost =
      config.provider === 'minio' || config.provider === 'rustfs'
        ? config.endpointHost
        : config.provider === 'aws'
          ? (config.endpointHost ?? undefined)
          : undefined;

    invoke<UploadResult>(command, {
      taskId: task.id,
      filePath: task.filePath,
      key,
      contentType: task.contentType,
      accountId: config.accountId,
      bucket: config.bucket,
      accessKeyId: config.accessKeyId,
      secretAccessKey: config.secretAccessKey,
      region: config.provider === 'aws' ? config.region : undefined,
      endpointScheme,
      endpointHost,
      forcePathStyle: config.provider === 'r2' ? undefined : config.forcePathStyle,
    })
      .then((result) => {
        if (result.success) {
          updateTask(task.id, { status: 'success', progress: 100, speed: 0 });
        } else {
          if (result.error?.includes('cancelled')) {
            updateTask(task.id, { status: 'cancelled', speed: 0 });
          } else {
            updateTask(task.id, {
              status: 'error',
              error: result.error || 'Upload failed',
              speed: 0,
            });
          }
        }
      })
      .catch((e) => {
        const errorMsg = e instanceof Error ? e.message : String(e);
        if (errorMsg.includes('cancelled')) {
          updateTask(task.id, { status: 'cancelled', speed: 0 });
        } else {
          updateTask(task.id, { status: 'error', error: errorMsg, speed: 0 });
        }
      })
      .finally(() => {
        isUploadingRef.current = false;
        if (unlistenRef.current) {
          unlistenRef.current();
          unlistenRef.current = null;
        }
      });

    return () => {
      // Cleanup: cancel upload if component unmounts during upload
      if (isUploadingRef.current) {
        invoke('cancel_upload', { taskId: task.id }).catch(() => {});
      }
      if (unlistenRef.current) {
        unlistenRef.current();
        unlistenRef.current = null;
      }
    };
  }, [
    task.status,
    task.id,
    task.filePath,
    task.fileName,
    task.contentType,
    config,
    uploadPath,
    updateTask,
  ]);

  function handleCancel() {
    invoke('cancel_upload', { taskId: task.id }).catch(() => {});
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
      : task.status === 'uploading'
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
            {task.speed > 0 ? (
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
    case 'error':
      return (
        <Text type="danger" style={{ fontSize: 12 }}>
          {task.error}
        </Text>
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
