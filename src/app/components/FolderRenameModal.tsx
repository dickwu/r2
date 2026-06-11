'use client';

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { App, Spin } from 'antd';
import { EditOutlined } from '@ant-design/icons';
import { listAllObjectsUnderPrefix, MoveOperation, StorageConfig } from '@/app/lib/r2cache';
import { runRenameBatch, useRenameStore } from '@/app/stores/renameStore';
import { formatEta } from '@/app/utils/formatBytes';
import { FileItem } from '@/app/hooks/useR2Files';
import type { FolderMetadata } from '@/app/stores/folderSizeStore';
import Modal from '@/app/components/ui/Modal';

interface FolderRenameModalProps {
  open: boolean;
  folder: FileItem | null;
  folderMetadata: FolderMetadata | undefined;
  config: StorageConfig | null | undefined;
  onClose: () => void;
  onSuccess: () => void;
}

function splitFolderKey(key: string) {
  const trimmed = key.endsWith('/') ? key.slice(0, -1) : key;
  if (!trimmed) return { parentPath: '', currentName: '' };
  const lastSlash = trimmed.lastIndexOf('/');
  if (lastSlash === -1) return { parentPath: '', currentName: trimmed };
  return {
    parentPath: trimmed.slice(0, lastSlash + 1),
    currentName: trimmed.slice(lastSlash + 1),
  };
}

/** Keep the tail of a long object key visible (the part being renamed). */
function truncateKey(key: string, max = 56): string {
  if (key.length <= max) return key;
  return `…${key.slice(key.length - max + 1)}`;
}

export default function FolderRenameModal({
  open,
  folder,
  folderMetadata,
  config,
  onClose,
  onSuccess,
}: FolderRenameModalProps) {
  const { message } = App.useApp();

  const { parentPath, currentName } = useMemo(() => {
    if (!folder?.key) return { parentPath: '', currentName: '' };
    return splitFolderKey(folder.key);
  }, [folder?.key]);

  const [newName, setNewName] = useState(currentName);
  const [confirmInput, setConfirmInput] = useState('');
  const [objectKeys, setObjectKeys] = useState<string[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [activeBatchId, setActiveBatchId] = useState<string | null>(null);
  const backgroundedRef = useRef(false);

  const batch = useRenameStore((s) =>
    activeBatchId ? s.batches.find((b) => b.id === activeBatchId) : undefined
  );
  const isRenaming = !!activeBatchId;

  const confirmCount = useMemo(() => {
    if (objectKeys.length > 0) return objectKeys.length;
    const expected = folderMetadata?.totalFileCount ?? folderMetadata?.fileCount;
    if (typeof expected === 'number') return expected;
    return 0;
  }, [folderMetadata, objectKeys.length]);

  useEffect(() => {
    if (!config || !folder?.key) return;

    const loadObjects = async () => {
      setIsLoading(true);
      try {
        const objects = await listAllObjectsUnderPrefix(config, folder.key);
        const keys = objects.map((obj) => obj.key);
        setObjectKeys(keys);
        if (keys.length === 0) {
          message.info('Folder is empty');
          onClose();
        }
      } catch (error) {
        message.error(
          `Failed to load folder contents: ${error instanceof Error ? error.message : 'Unknown error'}`
        );
        onClose();
      } finally {
        setIsLoading(false);
      }
    };

    loadObjects();
  }, [config, folder?.key, message, onClose]);

  const handleRename = useCallback(async () => {
    if (!config || !folder?.key || isRenaming || isLoading) return;

    const trimmedName = newName.trim();
    if (!trimmedName) {
      message.error('Folder name cannot be empty');
      return;
    }
    if (trimmedName.includes('/')) {
      message.error('Folder name cannot contain /');
      return;
    }

    const oldPrefix = folder.key.endsWith('/') ? folder.key : `${folder.key}/`;
    const newPrefix = `${parentPath}${trimmedName}/`;
    if (newPrefix === oldPrefix) {
      message.info('No changes made');
      onClose();
      return;
    }

    if (objectKeys.length === 0) {
      message.info('No files to rename');
      onClose();
      return;
    }

    if (confirmInput !== confirmCount.toString()) {
      message.error('Confirmation number does not match');
      return;
    }

    const operations: MoveOperation[] = objectKeys
      .filter((key) => key.startsWith(oldPrefix))
      .map((key) => ({
        old_key: key,
        new_key: `${newPrefix}${key.slice(oldPrefix.length)}`,
      }));

    if (operations.length === 0) {
      message.info('No files to rename');
      onClose();
      return;
    }

    backgroundedRef.current = false;
    const { id, done } = runRenameBatch(config, operations, `${currentName}/ → ${trimmedName}/`);
    setActiveBatchId(id);

    try {
      const result = await done;

      // If the user sent the batch to the background, the dock owns the
      // completion UX and the page-level effect refreshes the file list.
      if (backgroundedRef.current) return;

      setActiveBatchId(null);
      onClose();

      if (result.failed === 0) {
        message.success(`Renamed ${result.moved} file${result.moved !== 1 ? 's' : ''}`);
      } else if (result.moved > 0) {
        message.warning(
          `Renamed ${result.moved} file${result.moved !== 1 ? 's' : ''}, ${result.failed} failed`
        );
      } else {
        message.error('Failed to rename folder');
      }

      onSuccess();
    } catch (error) {
      if (backgroundedRef.current) return;
      setActiveBatchId(null);
      onClose();
      message.error(
        `Failed to rename folder: ${error instanceof Error ? error.message : 'Unknown error'}`
      );
    }
  }, [
    config,
    folder?.key,
    isRenaming,
    isLoading,
    newName,
    parentPath,
    currentName,
    objectKeys,
    confirmInput,
    confirmCount,
    message,
    onClose,
    onSuccess,
  ]);

  const handleContinueInBackground = useCallback(() => {
    backgroundedRef.current = true;
    message.info('Rename continues in the transfer panel');
    onClose();
  }, [message, onClose]);

  const completed = batch?.completed ?? 0;
  const totalOps = batch?.total ?? 0;
  const percent = totalOps > 0 ? Math.round((completed / totalOps) * 100) : 0;
  const confirmTarget = confirmCount.toLocaleString();
  const canConfirm =
    !isLoading && !isRenaming && objectKeys.length > 0 && confirmInput === confirmCount.toString();

  const renameStats: string[] = [];
  if (batch) {
    renameStats.push(`${completed.toLocaleString()} / ${totalOps.toLocaleString()} files`);
    if (batch.opsPerSec > 0.1) renameStats.push(`${batch.opsPerSec.toFixed(1)} files/s`);
    if (batch.etaMs > 0) renameStats.push(formatEta(batch.etaMs / 1000));
    if (batch.failed > 0) renameStats.push(`${batch.failed} failed`);
  }

  const footer = isRenaming ? (
    <button className="btn" onClick={handleContinueInBackground}>
      Continue in background
    </button>
  ) : (
    <>
      <button className="btn" onClick={onClose} disabled={isLoading}>
        Cancel
      </button>
      <button
        className="btn btn-primary"
        onClick={handleRename}
        disabled={!canConfirm || !newName.trim()}
      >
        Rename
      </button>
    </>
  );

  return (
    <Modal
      open={open}
      onClose={isRenaming ? handleContinueInBackground : onClose}
      title="Rename folder"
      icon={<EditOutlined style={{ fontSize: 18 }} />}
      width={480}
      footer={footer}
    >
      {isRenaming ? (
        <div style={{ padding: '8px 0 4px' }}>
          <div className="rename-progress-bar">
            <div className="rename-progress-fill active" style={{ width: `${percent}%` }} />
          </div>
          <div className="rename-progress-stats">
            <span>{renameStats.join(' · ')}</span>
            <span>{percent}%</span>
          </div>
          {batch?.currentKey && (
            <p className="rename-progress-current" title={batch.currentKey}>
              {truncateKey(batch.currentKey)}
            </p>
          )}
        </div>
      ) : isLoading ? (
        <div style={{ padding: '16px 0', textAlign: 'center' }}>
          <Spin />
          <p style={{ marginTop: 12, fontSize: 12.5, color: 'var(--text-muted)' }}>
            Loading folder contents…
          </p>
        </div>
      ) : (
        <>
          <div className="field">
            <div className="field-label">New name</div>
            <input
              className="input mono"
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
              autoFocus
              autoComplete="off"
              autoCorrect="off"
              autoCapitalize="off"
              disabled={isLoading}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && canConfirm && newName.trim()) handleRename();
              }}
            />
            <div className="field-hint">Renaming triggers a copy + delete on object storage.</div>
          </div>

          <div className="field" style={{ marginBottom: 0 }}>
            <div className="field-label">Confirm ({confirmTarget} files)</div>
            <input
              className="input mono"
              type="number"
              value={confirmInput}
              onChange={(e) => setConfirmInput(e.target.value)}
              placeholder={`Type ${confirmTarget} to confirm`}
              disabled={isLoading}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && canConfirm && newName.trim()) handleRename();
              }}
            />
            <div className="field-hint">
              This folder contains <strong>{confirmTarget}</strong> file
              {confirmCount !== 1 ? 's' : ''}. Type that number to confirm the rename.
            </div>
          </div>
        </>
      )}
    </Modal>
  );
}
