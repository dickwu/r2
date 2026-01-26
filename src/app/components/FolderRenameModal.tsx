'use client';

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Modal, Input, Progress, Button, App, Spin } from 'antd';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import {
  batchMoveObjects,
  listAllObjectsUnderPrefix,
  MoveOperation,
  StorageConfig,
} from '@/app/lib/r2cache';
import { FileItem } from '@/app/hooks/useR2Files';
import type { FolderMetadata } from '@/app/stores/folderSizeStore';

interface BatchMoveProgress {
  completed: number;
  total: number;
  failed: number;
}

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
  if (!trimmed) {
    return { parentPath: '', currentName: '' };
  }
  const lastSlash = trimmed.lastIndexOf('/');
  if (lastSlash === -1) {
    return { parentPath: '', currentName: trimmed };
  }
  return {
    parentPath: trimmed.slice(0, lastSlash + 1),
    currentName: trimmed.slice(lastSlash + 1),
  };
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
  const [newName, setNewName] = useState('');
  const [confirmInput, setConfirmInput] = useState('');
  const [objectKeys, setObjectKeys] = useState<string[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [isRenaming, setIsRenaming] = useState(false);
  const [progress, setProgress] = useState({ completed: 0, total: 0 });
  // Use ref instead of state to track if loaded, avoids useEffect dep array issues
  const hasLoadedRef = useRef(false);

  const { parentPath, currentName } = useMemo(() => {
    if (!folder?.key) return { parentPath: '', currentName: '' };
    return splitFolderKey(folder.key);
  }, [folder?.key]);

  const confirmCount = useMemo(() => {
    if (objectKeys.length > 0) return objectKeys.length;
    const expected = folderMetadata?.totalFileCount ?? folderMetadata?.fileCount;
    if (typeof expected === 'number') return expected;
    return 0;
  }, [folderMetadata, objectKeys.length]);

  useEffect(() => {
    if (!open) {
      setNewName('');
      setConfirmInput('');
      setObjectKeys([]);
      setProgress({ completed: 0, total: 0 });
      setIsLoading(false);
      setIsRenaming(false);
      hasLoadedRef.current = false;
      return;
    }

    // Skip if already loaded for this modal instance (prevents reload after rename)
    if (hasLoadedRef.current) return;

    setNewName(currentName);
    setConfirmInput('');
    setProgress({ completed: 0, total: 0 });

    if (!config || !folder?.key) return;

    const loadObjects = async () => {
      setIsLoading(true);
      try {
        const objects = await listAllObjectsUnderPrefix(config, folder.key);
        const keys = objects.map((obj) => obj.key);
        setObjectKeys(keys);
        hasLoadedRef.current = true;
        if (keys.length === 0) {
          message.info('Folder is empty');
          onClose();
        }
      } catch (error) {
        console.error('Failed to load folder contents:', error);
        message.error(
          `Failed to load folder contents: ${error instanceof Error ? error.message : 'Unknown error'}`
        );
        onClose();
      } finally {
        setIsLoading(false);
      }
    };

    loadObjects();
  }, [open, config, folder?.key, currentName, message, onClose]);

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

    setIsRenaming(true);
    setProgress({ completed: 0, total: operations.length });

    let unlisten: UnlistenFn | undefined;
    try {
      unlisten = await listen<BatchMoveProgress>('batch-move-progress', (event) => {
        setProgress({ completed: event.payload.completed, total: event.payload.total });
      });

      const result = await batchMoveObjects(config, operations);

      unlisten?.();
      setIsRenaming(false);
      onClose();

      if (result.failed === 0) {
        message.success(`Renamed ${result.moved} file${result.moved !== 1 ? 's' : ''}`);
      } else if (result.moved > 0) {
        message.warning(
          `Renamed ${result.moved} file${result.moved !== 1 ? 's' : ''}, ${result.failed} failed`
        );
        if (result.errors.length > 0) {
          console.error('Folder rename errors:', result.errors);
        }
      } else {
        message.error('Failed to rename folder');
      }

      onSuccess();
    } catch (error) {
      unlisten?.();
      console.error('Folder rename error:', error);
      setIsRenaming(false);
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
    objectKeys,
    confirmInput,
    confirmCount,
    message,
    onClose,
    onSuccess,
  ]);

  const percent = progress.total > 0 ? Math.round((progress.completed / progress.total) * 100) : 0;
  const confirmTarget = confirmCount.toLocaleString();
  const canConfirm =
    !isLoading && !isRenaming && objectKeys.length > 0 && confirmInput === confirmCount.toString();

  const footer = isRenaming ? null : (
    <>
      <Button onClick={onClose} disabled={isLoading}>
        Cancel
      </Button>
      <Button type="primary" onClick={handleRename} disabled={!canConfirm || !newName.trim()}>
        Rename
      </Button>
    </>
  );

  return (
    <Modal
      title={isRenaming ? 'Renaming Folder...' : 'Rename Folder'}
      open={open}
      onCancel={isRenaming ? undefined : onClose}
      footer={footer}
      closable={!isRenaming}
      maskClosable={!isRenaming}
    >
      {isRenaming ? (
        <div style={{ padding: '16px 0' }}>
          <Progress percent={percent} status="active" />
          <p style={{ marginTop: 12, textAlign: 'center', color: '#666' }}>
            {progress.completed} / {progress.total} files renamed
          </p>
        </div>
      ) : isLoading ? (
        <div style={{ padding: '16px 0', textAlign: 'center' }}>
          <Spin />
          <p style={{ marginTop: 12, color: '#666' }}>Loading folder contents...</p>
        </div>
      ) : (
        <>
          <p>
            Rename folder <strong>{currentName}</strong>
            {parentPath ? ` in /${parentPath}` : ' in / (root)'}.
          </p>
          <Input
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            placeholder="New folder name"
            autoFocus
            disabled={isLoading}
          />
          <p style={{ marginTop: 16 }}>
            This folder contains <strong>{confirmTarget}</strong> file
            {confirmCount !== 1 ? 's' : ''}. Type <strong>{confirmTarget}</strong> to confirm:
          </p>
          <Input
            value={confirmInput}
            onChange={(e) => setConfirmInput(e.target.value)}
            placeholder={`Type ${confirmTarget} to confirm`}
            disabled={isLoading}
            onPressEnter={() => {
              if (canConfirm && newName.trim()) {
                handleRename();
              }
            }}
          />
        </>
      )}
    </Modal>
  );
}
