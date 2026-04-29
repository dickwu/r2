'use client';

import { useState, useEffect, useCallback } from 'react';
import { Progress, App } from 'antd';
import { ExclamationCircleOutlined } from '@ant-design/icons';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { batchDeleteObjects, StorageConfig } from '@/app/lib/r2cache';
import Modal from '@/app/components/ui/Modal';

interface BatchDeleteProgress {
  completed: number;
  total: number;
  failed: number;
}

interface BatchDeleteModalProps {
  open: boolean;
  selectedKeys: Set<string>;
  config: StorageConfig | null | undefined;
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
  const [isDeleting, setIsDeleting] = useState(false);
  const [progress, setProgress] = useState({ completed: 0, total: 0 });
  const { message } = App.useApp();

  const selectedCount = selectedKeys.size;

  useEffect(() => {
    onDeletingChange?.(isDeleting);
  }, [isDeleting, onDeletingChange]);

  const handleDelete = useCallback(async () => {
    if (!config || selectedKeys.size === 0) return;

    const keys = Array.from(selectedKeys);
    const total = keys.length;

    setIsDeleting(true);
    setProgress({ completed: 0, total });

    let unlisten: UnlistenFn | undefined;
    try {
      unlisten = await listen<BatchDeleteProgress>('batch-delete-progress', (event) => {
        setProgress({ completed: event.payload.completed, total: event.payload.total });
      });

      const result = await batchDeleteObjects(config, keys);

      unlisten?.();
      setIsDeleting(false);
      onDeletingChange?.(false);
      onClose();

      if (result.failed === 0) {
        message.success(`Deleted ${result.deleted} file${result.deleted > 1 ? 's' : ''}`);
      } else {
        message.warning(`Deleted ${result.deleted} of ${total} files. ${result.failed} failed.`);
      }
      onSuccess();
    } catch (e) {
      unlisten?.();
      setIsDeleting(false);
      onDeletingChange?.(false);
      onClose();
      message.error(`Failed to delete files: ${e instanceof Error ? e.message : 'Unknown error'}`);
    }
  }, [config, selectedKeys, message, onClose, onSuccess, onDeletingChange]);

  const percent = progress.total > 0 ? Math.round((progress.completed / progress.total) * 100) : 0;

  const footer = isDeleting ? null : (
    <>
      <button className="btn" onClick={onClose}>
        Cancel
      </button>
      <button className="btn btn-danger" onClick={handleDelete}>
        Delete permanently
      </button>
    </>
  );

  return (
    <Modal
      open={open}
      onClose={isDeleting ? () => undefined : onClose}
      title={`Delete ${selectedCount} item${selectedCount !== 1 ? 's' : ''}?`}
      subtitle="This action cannot be undone."
      icon={<ExclamationCircleOutlined style={{ fontSize: 18, color: '#d4493a' }} />}
      width={480}
      footer={footer}
    >
      {isDeleting ? (
        <div style={{ padding: '8px 0' }}>
          <Progress percent={percent} status="active" />
          <p
            style={{
              marginTop: 12,
              textAlign: 'center',
              fontSize: 12.5,
              color: 'var(--text-muted)',
            }}
          >
            {progress.completed} / {progress.total} files deleted
          </p>
        </div>
      ) : (
        <>
          {/* Warning panel */}
          <div
            style={{
              padding: '12px 14px',
              background: 'rgba(212,73,58,0.08)',
              border: '1px solid rgba(212,73,58,0.22)',
              borderRadius: 9,
              marginBottom: 14,
              display: 'flex',
              gap: 12,
              alignItems: 'flex-start',
            }}
          >
            <ExclamationCircleOutlined
              style={{ fontSize: 16, color: '#d4493a', marginTop: 1, flexShrink: 0 }}
            />
            <div style={{ fontSize: 12.5, color: 'var(--text)', lineHeight: 1.55 }}>
              <div style={{ fontWeight: 600, marginBottom: 4 }}>
                Files in object storage are deleted immediately.
              </div>
              <div style={{ color: 'var(--text-muted)' }}>
                R2 / S3 do not have a recycle bin. Once deleted, these{' '}
                <strong>{selectedCount}</strong> file{selectedCount !== 1 ? 's' : ''} cannot be
                recovered.
              </div>
            </div>
          </div>

          <p style={{ fontSize: 13, color: 'var(--text-muted)', margin: 0 }}>
            Confirm by clicking <strong style={{ color: '#d4493a' }}>Delete permanently</strong>{' '}
            below.
          </p>
        </>
      )}
    </Modal>
  );
}
