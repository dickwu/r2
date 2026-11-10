'use client';

import { useTranslation } from 'react-i18next';
import { useEffect, useState } from 'react';
import { App, Spin } from 'antd';
import { EditOutlined, FolderOutlined, SwapOutlined } from '@ant-design/icons';
import FolderPickerModal from '@/app/components/folder/FolderPickerModal';
import { renameObject, StorageConfig } from '@/app/lib/r2cache';
import type { FileItem } from '@/app/hooks/useR2Files';
import Modal from '@/app/components/ui/Modal';

export interface FileRenameModalProps {
  open: boolean;
  onClose: () => void;
  file: FileItem;
  config: StorageConfig;
  onSuccess?: () => void;
}

export default function FileRenameModal({
  open,
  onClose,
  file,
  config,
  onSuccess,
}: FileRenameModalProps) {
  const [loading, setLoading] = useState(false);
  const [elapsedSec, setElapsedSec] = useState(0);
  const [folderPickerOpen, setFolderPickerOpen] = useState(false);
  const { message } = App.useApp();
  const { t } = useTranslation();

  // Elapsed-time ticker while the remote copy + delete runs (no byte-level
  // progress exists for a server-side CopyObject, so show time instead).
  if (!loading && elapsedSec !== 0) {
    setElapsedSec(0);
  }

  useEffect(() => {
    if (!loading) return;
    const startedAt = Date.now();
    const timer = setInterval(() => {
      setElapsedSec(Math.floor((Date.now() - startedAt) / 1000));
    }, 1000);
    return () => clearInterval(timer);
  }, [loading]);

  const currentDir = file.key.substring(0, file.key.lastIndexOf('/') + 1);
  const currentDirDisplay = currentDir ? currentDir.replace(/\/$/, '') : '';
  const [directory, setDirectory] = useState(currentDirDisplay);
  const [filename, setFilename] = useState(file.name);

  const handleOk = async () => {
    const trimmedFilename = filename.trim();
    if (!trimmedFilename) {
      message.error(t('rename.emptyName'));
      return;
    }
    if (trimmedFilename.includes('/')) {
      message.error(t('rename.noSlash'));
      return;
    }

    let dir = directory ? directory.trim() : '';
    if (dir && !dir.endsWith('/')) dir += '/';

    const newPath = dir ? `${dir}${trimmedFilename}` : trimmedFilename;
    if (newPath === file.key) {
      message.info(t('rename.noChanges'));
      onClose();
      return;
    }

    setLoading(true);
    try {
      await renameObject(config, file.key, newPath);
      message.success(t('rename.fileSuccess'));
      onSuccess?.();
      onClose();
    } catch (error) {
      if (error instanceof Error) {
        message.error(t('rename.fileFailed', { error: error.message }));
      }
    } finally {
      setLoading(false);
    }
  };

  const footer = (
    <>
      <button className="btn" onClick={onClose} disabled={loading}>
        {t('common.cancel')}
      </button>
      <button className="btn btn-primary" onClick={handleOk} disabled={loading || !filename.trim()}>
        {loading ? t('rename.renaming') : t('files.menu.rename')}
      </button>
    </>
  );

  return (
    <>
      <Modal
        open={open}
        onClose={loading ? () => undefined : onClose}
        title={t('rename.fileTitle')}
        icon={<EditOutlined style={{ fontSize: 18 }} />}
        width={480}
        footer={footer}
      >
        <div className="field">
          <div className="field-label">{t('rename.newName')}</div>
          <input
            className="input mono"
            value={filename}
            onChange={(e) => setFilename(e.target.value)}
            // oxlint-disable-next-line jsx-a11y/no-autofocus -- Dialog initial focus; ARIA APG asks that focus move into the dialog on open.
            autoFocus
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="off"
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !loading) handleOk();
            }}
            disabled={loading}
          />
          <div className="field-hint">{t('rename.hint')}</div>
        </div>

        <div className="field" style={{ marginBottom: 0 }}>
          <div className="field-label">{t('rename.location')}</div>
          <div
            style={{
              padding: '8px 12px',
              background: 'var(--bg-sunken)',
              borderRadius: 8,
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'space-between',
              gap: 8,
            }}
          >
            <div style={{ display: 'flex', alignItems: 'center', gap: 8, flex: 1, minWidth: 0 }}>
              <FolderOutlined style={{ color: 'var(--text-muted)', flexShrink: 0 }} />
              <span
                style={{
                  fontFamily: 'var(--font-mono)',
                  fontSize: 12,
                  overflow: 'hidden',
                  textOverflow: 'ellipsis',
                  whiteSpace: 'nowrap',
                  color: 'var(--text)',
                }}
              >
                {directory ? `/${directory}/` : t('rename.root')}
              </span>
            </div>
            <button
              className="btn"
              style={{ height: 26, fontSize: 11.5, padding: '0 10px' }}
              onClick={() => setFolderPickerOpen(true)}
              disabled={loading}
            >
              <SwapOutlined /> {t('rename.moveTo')}
            </button>
          </div>
        </div>

        {loading && (
          <div className="rename-busy">
            <Spin size="small" />
            <span>{t('rename.copyingRemote')}</span>
            {elapsedSec > 0 && <span className="rename-busy-elapsed">{elapsedSec}s</span>}
          </div>
        )}
      </Modal>

      {folderPickerOpen && (
        <FolderPickerModal
          open={true}
          onClose={() => setFolderPickerOpen(false)}
          selectedPath={directory}
          onConfirm={(path: string) => setDirectory(path)}
          title={t('rename.moveToFolder')}
        />
      )}
    </>
  );
}
