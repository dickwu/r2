'use client';

import { useState, useEffect, useCallback } from 'react';
import { Modal, Input, Progress, Button, App } from 'antd';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { batchDeleteR2Objects, R2Config } from '../lib/r2cache';

interface BatchDeleteProgress {
  completed: number;
  total: number;
  failed: number;
}

interface BatchDeleteModalProps {
  open: boolean;
  selectedKeys: Set<string>;
  config: R2Config | null | undefined;
  onClose: () => void;
  onSuccess: () => void;
  onDeletingChange?: (isDeleting: boolean) => void;
}

export default function BatchDeleteModal({
  open,
  selectedKeys,
  config,
  onClose,
  onSuccess,
  onDeletingChange,
}: BatchDeleteModalProps) {
  const [confirmInput, setConfirmInput] = useState('');
  const [isDeleting, setIsDeleting] = useState(false);
  const [progress, setProgress] = useState({ completed: 0, total: 0 });
  const { message } = App.useApp();

  const selectedCount = selectedKeys.size;

  // Reset state when modal opens/closes
  useEffect(() => {
    if (!open) {
      setConfirmInput('');
      setProgress({ completed: 0, total: 0 });
    }
  }, [open]);

  // Notify parent of deleting state changes
  useEffect(() => {
    onDeletingChange?.(isDeleting);
  }, [isDeleting, onDeletingChange]);

  const handleDelete = useCallback(async () => {
    if (!config || selectedKeys.size === 0) return;

    const keys = Array.from(selectedKeys);
    const total = keys.length;

    setIsDeleting(true);
    setProgress({ completed: 0, total });

    // Listen for progress events from Rust backend
    let unlisten: UnlistenFn | undefined;
    try {
      unlisten = await listen<BatchDeleteProgress>('batch-delete-progress', (event) => {
        setProgress({ completed: event.payload.completed, total: event.payload.total });
      });

      // Use Rust batch delete API
      const result = await batchDeleteR2Objects(config, keys);

      // Cleanup listener
      unlisten?.();

      setIsDeleting(false);
      onDeletingChange?.(false);
      onClose();

      if (result.failed === 0) {
        message.success(`Deleted ${result.deleted} file${result.deleted > 1 ? 's' : ''}`);
      } else {
        message.warning(`Deleted ${result.deleted} of ${total} files. ${result.failed} failed.`);
        if (result.errors.length > 0) {
          console.error('Batch delete errors:', result.errors);
        }
      }
      onSuccess();
    } catch (e) {
      unlisten?.();
      console.error('Batch delete error:', e);
      setIsDeleting(false);
      onDeletingChange?.(false);
      onClose();
      message.error(`Failed to delete files: ${e instanceof Error ? e.message : 'Unknown error'}`);
    }
  }, [config, selectedKeys, message, onClose, onSuccess, onDeletingChange]);

  const percent = progress.total > 0 ? Math.round((progress.completed / progress.total) * 100) : 0;

  const confirmFooter = (
    <>
      <Button onClick={onClose}>Cancel</Button>
      <Button
        type="primary"
        danger
        disabled={confirmInput !== selectedCount.toString()}
        onClick={handleDelete}
      >
        Delete
      </Button>
    </>
  );

  return (
    <Modal
      title={isDeleting ? 'Deleting Files...' : 'Confirm Batch Delete'}
      open={open}
      onCancel={isDeleting ? undefined : onClose}
      footer={isDeleting ? null : confirmFooter}
      closable={!isDeleting}
      maskClosable={!isDeleting}
    >
      {isDeleting ? (
        <div style={{ padding: '16px 0' }}>
          <Progress percent={percent} status="active" />
          <p style={{ marginTop: 12, textAlign: 'center', color: '#666' }}>
            {progress.completed} / {progress.total} files deleted
          </p>
        </div>
      ) : (
        <>
          <p>
            You are about to delete <strong>{selectedCount}</strong> file
            {selectedCount > 1 ? 's' : ''}.
          </p>
          <p>This action cannot be undone.</p>
          <p style={{ marginTop: 16, marginBottom: 8 }}>
            Please type <strong>{selectedCount}</strong> to confirm:
          </p>
          <Input
            value={confirmInput}
            onChange={(e) => setConfirmInput(e.target.value)}
            placeholder={`Type ${selectedCount} to confirm`}
            autoFocus
            onPressEnter={() => {
              if (confirmInput === selectedCount.toString()) {
                handleDelete();
              }
            }}
          />
        </>
      )}
    </Modal>
  );
}
