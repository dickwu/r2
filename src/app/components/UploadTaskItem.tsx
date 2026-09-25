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
import { uploadFile, type StorageConfig } from '@/app/lib/r2cache';
import {
  accountDisplayName,
  preferRecorded,
  uploadFailure,
  type UploadFailureContext,
} from '@/app/lib/taskFailures';
import { useAccountStore } from '@/app/stores/accountStore';
import { useTransferErrorStore } from '@/app/stores/transferErrorStore';
import { useUploadStore, type UploadTask } from '@/app/stores/uploadStore';

const { Text } = Typography;

interface UploadProgress {
  task_id: string;
  percent: number;
  uploaded_bytes: number;
  total_bytes: number;
  speed: number;
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

    const key = uploadKey(uploadPath, task);
    // A failed upload is recorded on the task and reported to the failure modal,
    // with the destination this upload used
    const fail = (error: string) => {
      updateTask(task.id, { status: 'error', error, speed: 0 });
      reportUploadFailure(task.id, uploadContext(config, key));
    };

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

    uploadFile(config, {
      taskId: task.id,
      filePath: task.filePath,
      key,
      contentType: task.contentType,
    })
      .then((result) => {
        if (result.success) {
          updateTask(task.id, { status: 'success', progress: 100, speed: 0 });
        } else {
          if (result.error?.includes('cancelled')) {
            updateTask(task.id, { status: 'cancelled', speed: 0 });
          } else {
            fail(result.error || 'Upload failed');
          }
        }
      })
      .catch((e) => {
        const errorMsg = e instanceof Error ? e.message : String(e);
        if (errorMsg.includes('cancelled')) {
          updateTask(task.id, { status: 'cancelled', speed: 0 });
        } else {
          fail(errorMsg);
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

// The object key an upload writes: the destination folder, then the file's
// (possibly auto-renamed) name
function uploadKey(uploadPath: string, task: UploadTask): string {
  const folder = uploadPath && !uploadPath.endsWith('/') ? `${uploadPath}/` : uploadPath;
  return folder + (task.renamedFileName ?? task.fileName);
}

// Where an upload is going, as its failure record names it
function uploadContext(config: StorageConfig | null, key: string): UploadFailureContext {
  if (!config) return { key };
  const { accounts } = useAccountStore.getState();
  return {
    key,
    bucket: config.bucket,
    account: accountDisplayName(accounts, config.provider, config.accountId),
  };
}

// Report the upload that just failed, read back from the store so the record
// carries the last progress the row showed
function reportUploadFailure(taskId: string, context: UploadFailureContext) {
  const failed = useUploadStore.getState().tasks.find((t) => t.id === taskId);
  if (failed) useTransferErrorStore.getState().report(uploadFailure(failed, context));
}

// "Show error" re-opens the reported record, which holds the destination the
// upload used; it rebuilds one only after the failure modal's list was cleared
function showUploadFailure(task: UploadTask) {
  const { config, uploadPath } = useUploadStore.getState();
  const fresh = uploadFailure(task, uploadContext(config, uploadKey(uploadPath, task)));
  const { failures, show } = useTransferErrorStore.getState();
  show(preferRecorded(fresh, failures));
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
