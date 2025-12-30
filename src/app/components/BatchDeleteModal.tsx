'use client';

import { useState, useEffect, useCallback } from 'react';
import { Modal, Input, App } from 'antd';
import { deleteR2Object, R2Config } from '../lib/r2cache';

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
  const { message } = App.useApp();

  const selectedCount = selectedKeys.size;

  // Reset input when modal opens/closes
  useEffect(() => {
    if (!open) {
      setConfirmInput('');
    }
  }, [open]);

  // Notify parent of deleting state changes
  useEffect(() => {
    onDeletingChange?.(isDeleting);
  }, [isDeleting, onDeletingChange]);

  const handleDelete = useCallback(async () => {
    if (!config || selectedKeys.size === 0) return;

    const keys = Array.from(selectedKeys);
    const count = keys.length;

    // Close modal immediately
    onClose();

    // Start deleting
    setIsDeleting(true);

    try {
      await Promise.all(keys.map((key) => deleteR2Object(config, key)));
      message.success(`Deleted ${count} file${count > 1 ? 's' : ''}`);
      onSuccess();
    } catch (e) {
      console.error('Batch delete error:', e);
      message.error(`Failed to delete files: ${e instanceof Error ? e.message : 'Unknown error'}`);
    } finally {
      setIsDeleting(false);
    }
  }, [config, selectedKeys, message, onClose, onSuccess]);

  const handleOk = () => {
    if (confirmInput === selectedCount.toString()) {
      handleDelete();
    }
  };

  return (
    <Modal
      title="Confirm Batch Delete"
      open={open}
      onCancel={onClose}
      onOk={handleOk}
      okText="Delete"
      okButtonProps={{
        danger: true,
        disabled: confirmInput !== selectedCount.toString(),
      }}
    >
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
        onPressEnter={handleOk}
      />
    </Modal>
  );
}
