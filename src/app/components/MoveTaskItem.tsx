'use client';

import { memo } from 'react';
import { Button, Progress, Typography, Space, Tooltip } from 'antd';
import {
  CheckCircleOutlined,
  ClockCircleOutlined,
  CloseCircleOutlined,
  LoadingOutlined,
  PauseCircleOutlined,
  PlayCircleOutlined,
  DeleteOutlined,
  StopOutlined,
  SwapOutlined,
} from '@ant-design/icons';
import { invoke } from '@tauri-apps/api/core';
import { type MoveTask } from '@/app/stores/moveStore';
import { formatBytes } from '@/app/utils/formatBytes';

const { Text } = Typography;

interface MoveTaskItemProps {
  task: MoveTask;
  onResume?: () => void;
}

function MoveTaskItem({ task, onResume }: MoveTaskItemProps) {
  async function handlePause() {
    try {
      await invoke('pause_move', { taskId: task.id });
    } catch (e) {
      console.error('Failed to pause move:', e);
    }
  }

  async function handleCancel() {
    try {
      await invoke('cancel_move', { taskId: task.id });
    } catch (e) {
      console.error('Failed to cancel move:', e);
    }
  }

  async function handleDelete() {
    try {
      await invoke('delete_move_task', { taskId: task.id });
    } catch (e) {
      console.error('Failed to delete move task:', e);
    }
  }

  function handleResume() {
    if (onResume) onResume();
  }

  // Upload at 100% = finishing (post-sync)
  const isUploadFinishing = task.status === 'uploading' && task.progress >= 100;
  const isFinishing = task.status === 'finishing' || task.status === 'deleting' || isUploadFinishing;

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
              <Button type="text" size="small" icon={<PauseCircleOutlined />} onClick={handlePause} />
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
      case 'uploading':
        // If upload is at 100%, show only cancel (finishing state)
        if (isUploadFinishing) {
          return (
            <Space size={4}>
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
        }
        return (
          <Space size={4}>
            <Tooltip title="Pause">
              <Button type="text" size="small" icon={<PauseCircleOutlined />} onClick={handlePause} />
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
      case 'finishing':
      case 'deleting':
        // Post-processing (finishing) - only cancel, no pause
        return (
          <Space size={4}>
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
      <StatusIcon status={task.status} progress={task.progress} />
      <div style={{ flex: 1, minWidth: 0 }}>
        <Text ellipsis style={{ maxWidth: 320, display: 'block' }}>
          {task.sourceKey}
        </Text>
        <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
          <SwapOutlined style={{ color: '#999', fontSize: 12 }} />
          <Text ellipsis type="secondary" style={{ maxWidth: 280, fontSize: 12 }}>
            {task.destBucket}/{task.destKey}
          </Text>
        </div>
        <TaskDescription task={task} />
      </div>
      <div style={{ display: 'flex', gap: 4 }}>{getActions()}</div>
    </div>
  );
}

function StatusIcon({ status, progress }: { status: MoveTask['status']; progress: number }) {
  // Uploading at 100% = finishing (waiting for post-sync)
  const isFinishing = status === 'finishing' || status === 'deleting' || (status === 'uploading' && progress >= 100);
  
  switch (status) {
    case 'success':
      return <CheckCircleOutlined style={{ color: '#52c41a', fontSize: 16 }} />;
    case 'error':
      return <CloseCircleOutlined style={{ color: '#ff4d4f', fontSize: 16 }} />;
    case 'cancelled':
      return <StopOutlined style={{ color: '#999', fontSize: 16 }} />;
    case 'downloading':
      return <LoadingOutlined style={{ color: '#1677ff', fontSize: 16 }} />;
    case 'uploading':
      // Show checkmark when upload complete (100%), spinner when still uploading
      return isFinishing ? (
        <CheckCircleOutlined style={{ color: '#52c41a', fontSize: 16, opacity: 0.7 }} />
      ) : (
        <LoadingOutlined style={{ color: '#1677ff', fontSize: 16 }} />
      );
    case 'finishing':
      return <CheckCircleOutlined style={{ color: '#52c41a', fontSize: 16, opacity: 0.7 }} />;
    case 'deleting':
      // Finishing (post-processing) - transfer complete, cleaning up
      return <CheckCircleOutlined style={{ color: '#52c41a', fontSize: 16, opacity: 0.7 }} />;
    case 'paused':
      return <PauseCircleOutlined style={{ color: '#faad14', fontSize: 16 }} />;
    case 'pending':
      return <ClockCircleOutlined style={{ color: '#999', fontSize: 16 }} />;
    default:
      return null;
  }
}

function TaskDescription({ task }: { task: MoveTask }) {
  // Upload at 100% = finishing (post-sync), not actively uploading
  const isUploadFinishing = task.status === 'uploading' && task.progress >= 100;
  
  switch (task.status) {
    case 'downloading':
    case 'uploading': {
      // Show "Finishing" when upload is at 100% (waiting for post-sync)
      if (isUploadFinishing) {
        return (
          <Text type="success" style={{ fontSize: 12 }}>
            Finishing (uploaded, syncing...)
          </Text>
        );
      }
      const phaseLabel = task.status === 'downloading' ? 'Downloading' : 'Uploading';
      return (
        <div>
          <Progress
            percent={Math.round(task.progress)}
            status="active"
            strokeColor={{ from: '#108ee9', to: '#87d068' }}
            railColor="#e8e8e8"
            size={['100%', 8]}
          />
          <Text type="secondary" style={{ fontSize: 11 }}>
            {phaseLabel}
            {task.speed > 0 && (
              <span style={{ marginLeft: 8 }}>{formatBytes(task.speed)}/s</span>
            )}
            {task.fileSize > 0 && (
              <span style={{ marginLeft: 8 }}>
                {formatBytes(task.transferredBytes)} / {formatBytes(task.fileSize)}
              </span>
            )}
          </Text>
        </div>
      );
    }
    case 'finishing':
      return (
        <Text type="success" style={{ fontSize: 12 }}>
          Finishing (uploaded, syncing...)
        </Text>
      );
    case 'deleting':
      // Post-processing phase - transfer complete, cleaning up
      return (
        <Text type="success" style={{ fontSize: 12 }}>
          Finishing (removing source)
        </Text>
      );
    case 'paused':
      return (
        <Text type="warning" style={{ fontSize: 12 }}>
          Paused at {Math.round(task.progress)}%
        </Text>
      );
    case 'error':
      return (
        <Text type="danger" style={{ fontSize: 12 }}>
          {task.error || 'Move failed'}
        </Text>
      );
    case 'success':
      return (
        <Text type="success" style={{ fontSize: 12 }}>
          Moved
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
          {task.fileSize > 0 ? formatBytes(task.fileSize) : 'Queued'}
        </Text>
      );
  }
}

// Memoize to prevent unnecessary re-renders in the list
export default memo(MoveTaskItem, (prevProps, nextProps) => {
  const prev = prevProps.task;
  const next = nextProps.task;
  // Only re-render if task data that affects display has changed
  return (
    prev.id === next.id &&
    prev.status === next.status &&
    prev.progress === next.progress &&
    prev.speed === next.speed &&
    prev.transferredBytes === next.transferredBytes &&
    prev.error === next.error &&
    prev.phase === next.phase
  );
});
