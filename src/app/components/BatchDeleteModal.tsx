'use client';

import { useState, useEffect, useCallback } from 'react';
import { App } from 'antd';
import { ExclamationCircleOutlined } from '@ant-design/icons';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { batchDeleteObjects, BatchDeleteResult, StorageConfig } from '@/app/lib/r2cache';
import type { TransferFailure } from '@/app/lib/transferFailure';
import { useTransferErrorStore } from '@/app/stores/transferErrorStore';
import Modal from '@/app/components/ui/Modal';

interface BatchDeleteProgress {
  completed: number;
  total: number;
  failed: number;
}

const plural = (n: number, noun: string) => `${noun}${n === 1 ? '' : 's'}`;

/**
 * The failure record for a delete that left objects behind. The backend sends
 * one reason per failed sub-batch, and every one of them reaches the modal.
 */
function deleteFailureRecord(
  result: BatchDeleteResult,
  total: number,
  bucket: string
): TransferFailure {
  const now = Date.now();
  const reasons = result.errors.join('\n');
  return {
    id: `delete:${now}`,
    kind: 'delete',
    name: `${result.failed} of ${total} ${plural(total, 'object')}`,
    message:
      reasons.trim() !== ''
        ? reasons
        : `${result.failed} ${plural(result.failed, 'object')} could not be deleted`,
    occurredAt: now,
    bucket,
    progress: { done: result.deleted, total },
  };
}

/** The strip's "N failed" count. Once the delete has finished it reopens the reasons. */
function FailedCount({ count, failure }: { count: number; failure: TransferFailure | null }) {
  const label = `${count.toLocaleString()} failed`;
  return (
    <span className="bp-failed">
      {' · '}
      {failure ? (
        <button
          type="button"
          className="bp-failed-btn"
          title="Show errors"
          aria-label={`${label}. Show errors`}
          onClick={() => useTransferErrorStore.getState().show(failure)}
        >
          {label}
        </button>
      ) : (
        label
      )}
    </span>
  );
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
  const [progress, setProgress] = useState({ completed: 0, total: 0, failed: 0 });
  // Set once a delete finishes with failures: the modal then stays on its final
  // strip, whose failed count reopens this record.
  const [failure, setFailure] = useState<TransferFailure | null>(null);
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
    setProgress({ completed: 0, total, failed: 0 });

    let unlisten: UnlistenFn | undefined;
    try {
      unlisten = await listen<BatchDeleteProgress>('batch-delete-progress', (event) => {
        setProgress({
          completed: event.payload.completed,
          total: event.payload.total,
          failed: event.payload.failed,
        });
      });

      const result = await batchDeleteObjects(config, keys);

      unlisten?.();
      setIsDeleting(false);
      onDeletingChange?.(false);

      if (result.failed === 0) {
        onClose();
        message.success(`Deleted ${result.deleted} file${result.deleted > 1 ? 's' : ''}`);
      } else {
        // Stay open on the final counts; the failure modal opens over this one
        // with the reasons, and the failed count reopens it after it closes.
        const record = deleteFailureRecord(result, total, config.bucket);
        setProgress({ completed: result.deleted, total, failed: result.failed });
        setFailure(record);
        useTransferErrorStore.getState().report(record);
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
  const showProgress = isDeleting || failure !== null;

  const footer = isDeleting ? null : failure ? (
    <button className="btn btn-primary" onClick={onClose}>
      Close
    </button>
  ) : (
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
      title={
        failure
          ? `Deleted ${progress.completed.toLocaleString()} of ${progress.total.toLocaleString()} ${plural(progress.total, 'item')}`
          : `Delete ${selectedCount} item${selectedCount !== 1 ? 's' : ''}?`
      }
      subtitle={
        failure
          ? `${progress.failed.toLocaleString()} could not be deleted.`
          : 'This action cannot be undone.'
      }
      icon={<ExclamationCircleOutlined style={{ fontSize: 18, color: '#d4493a' }} />}
      width={480}
      footer={footer}
    >
      {showProgress ? (
        <div className="batch-progress" role="status" aria-live="polite">
          <div
            className="batch-progress-track"
            role="progressbar"
            aria-valuenow={percent}
            aria-valuemin={0}
            aria-valuemax={100}
          >
            <div
              className={isDeleting ? 'batch-progress-fill active' : 'batch-progress-fill'}
              style={{ width: `${percent}%` }}
            />
          </div>
          <div className="batch-progress-stats">
            <span>
              {progress.completed.toLocaleString()} / {progress.total.toLocaleString()} deleted
              {progress.failed > 0 && <FailedCount count={progress.failed} failure={failure} />}
            </span>
            <span>{percent}%</span>
          </div>
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
