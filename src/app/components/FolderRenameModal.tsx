'use client';

import { t as i18nT } from '@/i18n';
import { Trans, useTranslation } from 'react-i18next';
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
  const { t } = useTranslation();

  const { parentPath, currentName } = folder?.key
    ? splitFolderKey(folder.key)
    : { parentPath: '', currentName: '' };

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
          message.info(i18nT('folderRename.empty'));
          onClose();
        }
      } catch (error) {
        message.error(
          i18nT('folderRename.loadFailed', {
            error: error instanceof Error ? error.message : i18nT('common.unknownError'),
          })
        );
        onClose();
      } finally {
        setIsLoading(false);
      }
    };

    loadObjects();
  }, [config, folder?.key, message, onClose]);

  const handleRename = async () => {
    if (!config || !folder?.key || isRenaming || isLoading) return;

    const trimmedName = newName.trim();
    if (!trimmedName) {
      message.error(i18nT('folderRename.emptyName'));
      return;
    }
    if (trimmedName.includes('/')) {
      message.error(i18nT('folderRename.noSlash'));
      return;
    }

    const oldPrefix = folder.key.endsWith('/') ? folder.key : `${folder.key}/`;
    const newPrefix = `${parentPath}${trimmedName}/`;
    if (newPrefix === oldPrefix) {
      message.info(i18nT('rename.noChanges'));
      onClose();
      return;
    }

    if (objectKeys.length === 0) {
      message.info(t('folderRename.noFiles'));
      onClose();
      return;
    }

    if (confirmInput !== confirmCount.toString()) {
      message.error(i18nT('folderRename.confirmMismatch'));
      return;
    }

    const operations: MoveOperation[] = objectKeys
      .filter((key) => key.startsWith(oldPrefix))
      .map((key) => ({
        old_key: key,
        new_key: `${newPrefix}${key.slice(oldPrefix.length)}`,
      }));

    if (operations.length === 0) {
      message.info(t('folderRename.noFiles'));
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
        message.success(i18nT('folderRename.renamed', { count: result.moved }));
      } else if (result.moved > 0) {
        message.warning(
          i18nT('folderRename.renamedPartial', { count: result.moved, failed: result.failed })
        );
      } else {
        message.error(i18nT('folderRename.failed'));
      }

      onSuccess();
    } catch (error) {
      if (backgroundedRef.current) return;
      setActiveBatchId(null);
      onClose();
      message.error(
        i18nT('folderRename.failedWith', {
          error: error instanceof Error ? error.message : i18nT('common.unknownError'),
        })
      );
    }
  };

  const handleContinueInBackground = useCallback(() => {
    backgroundedRef.current = true;
    message.info(i18nT('folderRename.continues'));
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
    renameStats.push(
      t('status.filesOf', {
        count: totalOps,
        done: completed.toLocaleString(),
        total: totalOps.toLocaleString(),
      })
    );
    if (batch.opsPerSec > 0.1)
      renameStats.push(t('folderRename.filesPerSec', { rate: batch.opsPerSec.toFixed(1) }));
    if (batch.etaMs > 0) renameStats.push(formatEta(batch.etaMs / 1000));
    if (batch.failed > 0) renameStats.push(t('batchDelete.failedCount', { count: batch.failed }));
  }

  const footer = isRenaming ? (
    <button className="btn" onClick={handleContinueInBackground}>
      {t('folderRename.background')}
    </button>
  ) : (
    <>
      <button className="btn" onClick={onClose} disabled={isLoading}>
        {t('common.cancel')}
      </button>
      <button
        className="btn btn-primary"
        onClick={handleRename}
        disabled={!canConfirm || !newName.trim()}
      >
        {t('files.action.rename')}
      </button>
    </>
  );

  return (
    <Modal
      open={open}
      onClose={isRenaming ? handleContinueInBackground : onClose}
      title={t('files.action.renameFolder')}
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
            {t('folderRename.loading')}
          </p>
        </div>
      ) : (
        <>
          <div className="field">
            <div className="field-label">{t('rename.newName')}</div>
            <input
              className="input mono"
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
              // oxlint-disable-next-line jsx-a11y/no-autofocus -- Dialog initial focus; ARIA APG asks that focus move into the dialog on open.
              autoFocus
              autoComplete="off"
              autoCorrect="off"
              autoCapitalize="off"
              disabled={isLoading}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && canConfirm && newName.trim()) handleRename();
              }}
            />
            <div className="field-hint">{t('rename.hint')}</div>
          </div>

          <div className="field" style={{ marginBottom: 0 }}>
            <div className="field-label">
              {t('folderRename.confirmLabel', { count: confirmCount, files: confirmTarget })}
            </div>
            <input
              className="input mono"
              type="number"
              value={confirmInput}
              onChange={(e) => setConfirmInput(e.target.value)}
              placeholder={t('folderRename.confirmPlaceholder', { number: confirmTarget })}
              disabled={isLoading}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && canConfirm && newName.trim()) handleRename();
              }}
            />
            <div className="field-hint">
              <Trans
                i18nKey="folderRename.confirmHint"
                count={confirmCount}
                values={{ files: confirmTarget }}
                components={{ strong: <strong /> }}
              />
            </div>
          </div>
        </>
      )}
    </Modal>
  );
}
