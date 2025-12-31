'use client';

import { useState, useEffect, useCallback } from 'react';
import { Modal, App, Progress } from 'antd';
import { FolderOutlined } from '@ant-design/icons';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { batchMoveR2Objects, MoveOperation, R2Config } from '../lib/r2cache';
import FolderTreePicker from './FolderTreePicker';

interface BatchMoveProgress {
  completed: number;
  total: number;
  failed: number;
}

interface BatchMoveModalProps {
  open: boolean;
  selectedKeys: Set<string>;
  config: R2Config | null | undefined;
  onClose: () => void;
  onSuccess: () => void;
  onMovingChange?: (isMoving: boolean) => void;
}

export default function BatchMoveModal({
  open,
  selectedKeys,
  config,
  onClose,
  onSuccess,
  onMovingChange,
}: BatchMoveModalProps) {
  const [targetDirectory, setTargetDirectory] = useState('');
  const [isMoving, setIsMoving] = useState(false);
  const [progress, setProgress] = useState({ current: 0, total: 0 });
  const { message } = App.useApp();

  const selectedCount = selectedKeys.size;

  // Reset state when modal opens/closes
  useEffect(() => {
    if (!open) {
      setTargetDirectory('');
      setProgress({ current: 0, total: 0 });
    }
  }, [open]);

  // Notify parent of moving state changes
  useEffect(() => {
    onMovingChange?.(isMoving);
  }, [isMoving, onMovingChange]);

  const handleMove = useCallback(async () => {
    if (!config || selectedKeys.size === 0) return;

    const keys = Array.from(selectedKeys);
    const total = keys.length;

    // Build move operations
    const operations: MoveOperation[] = keys.map((key) => {
      const filename = key.split('/').pop() || key;
      const newPath = targetDirectory ? `${targetDirectory}/${filename}` : filename;
      return { old_key: key, new_key: newPath };
    });

    setIsMoving(true);
    setProgress({ current: 0, total });

    // Listen for progress events from Rust backend
    let unlisten: UnlistenFn | undefined;
    try {
      unlisten = await listen<BatchMoveProgress>('batch-move-progress', (event) => {
        setProgress({ current: event.payload.completed, total: event.payload.total });
      });

      // Use Rust batch move API (6 concurrent operations)
      const result = await batchMoveR2Objects(config, operations);

      // Cleanup listener
      unlisten?.();

      setIsMoving(false);
      setProgress({ current: 0, total: 0 });
      onMovingChange?.(false);
      onClose();

      if (result.failed === 0) {
        message.success(`Moved ${result.moved} file${result.moved > 1 ? 's' : ''}`);
      } else if (result.moved > 0) {
        message.warning(
          `Moved ${result.moved} file${result.moved > 1 ? 's' : ''}, ${result.failed} failed`
        );
        if (result.errors.length > 0) {
          console.error('Batch move errors:', result.errors);
        }
      } else {
        message.error(`Failed to move files`);
      }
      onSuccess();
    } catch (e) {
      unlisten?.();
      console.error('Batch move error:', e);
      setIsMoving(false);
      setProgress({ current: 0, total: 0 });
      onMovingChange?.(false);
      onClose();
      message.error(`Failed to move files: ${e instanceof Error ? e.message : 'Unknown error'}`);
    }
  }, [config, selectedKeys, targetDirectory, message, onClose, onSuccess, onMovingChange]);

  const percent = progress.total > 0 ? Math.round((progress.current / progress.total) * 100) : 0;

  return (
    <Modal
      title={isMoving ? 'Moving Files...' : 'Move Files'}
      open={open}
      onCancel={isMoving ? undefined : onClose}
      onOk={handleMove}
      okText="Move"
      okButtonProps={{ disabled: isMoving }}
      cancelButtonProps={{ disabled: isMoving }}
      closable={!isMoving}
      maskClosable={!isMoving}
      footer={isMoving ? null : undefined}
      width="90%"
      style={{ top: '3%' }}
    >
      {isMoving ? (
        <div style={{ padding: '24px 0' }}>
          <Progress percent={percent} status="active" />
          <p style={{ marginTop: 12, textAlign: 'center', color: '#666' }}>
            {progress.current} / {progress.total} files moved
          </p>
        </div>
      ) : (
        <div style={{ height: '70vh', overflow: 'auto' }}>
          <p style={{ marginBottom: 16 }}>
            Move <strong>{selectedCount}</strong> file{selectedCount > 1 ? 's' : ''} to:
          </p>

          {/* Current target display */}
          <div
            style={{
              padding: '8px 12px',
              background: 'var(--bg-secondary)',
              borderRadius: 6,
              marginBottom: 16,
              display: 'flex',
              alignItems: 'center',
              gap: 8,
            }}
          >
            <FolderOutlined style={{ color: 'var(--text-secondary)' }} />
            <span style={{ fontFamily: 'monospace' }}>
              {targetDirectory ? `/${targetDirectory}/` : '/ (root)'}
            </span>
          </div>

          {/* Folder picker */}
          <FolderTreePicker selectedPath={targetDirectory} onSelect={setTargetDirectory} />
        </div>
      )}
    </Modal>
  );
}
