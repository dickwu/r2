'use client';

import { useState } from 'react';
import { App } from 'antd';
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
  const [folderPickerOpen, setFolderPickerOpen] = useState(false);
  const { message } = App.useApp();

  const currentDir = file.key.substring(0, file.key.lastIndexOf('/') + 1);
  const currentDirDisplay = currentDir ? currentDir.replace(/\/$/, '') : '';
  const [directory, setDirectory] = useState(currentDirDisplay);
  const [filename, setFilename] = useState(file.name);

  const handleOk = async () => {
    const trimmedFilename = filename.trim();
    if (!trimmedFilename) {
      message.error('File name cannot be empty');
      return;
    }
    if (trimmedFilename.includes('/')) {
      message.error('File name cannot contain /');
      return;
    }

    let dir = directory ? directory.trim() : '';
    if (dir && !dir.endsWith('/')) dir += '/';

    const newPath = dir ? `${dir}${trimmedFilename}` : trimmedFilename;
    if (newPath === file.key) {
      message.info('No changes made');
      onClose();
      return;
    }

    setLoading(true);
    try {
      await renameObject(config, file.key, newPath);
      message.success('File renamed/moved successfully');
      onSuccess?.();
      onClose();
    } catch (error) {
      if (error instanceof Error) {
        message.error(`Failed to rename/move: ${error.message}`);
      }
    } finally {
      setLoading(false);
    }
  };

  const footer = (
    <>
      <button className="btn" onClick={onClose} disabled={loading}>
        Cancel
      </button>
      <button className="btn btn-primary" onClick={handleOk} disabled={loading || !filename.trim()}>
        {loading ? 'Renaming…' : 'Rename'}
      </button>
    </>
  );

  return (
    <>
      <Modal
        open={open}
        onClose={loading ? () => undefined : onClose}
        title="Rename file"
        icon={<EditOutlined style={{ fontSize: 18 }} />}
        width={480}
        footer={footer}
      >
        <div className="field">
          <div className="field-label">New name</div>
          <input
            className="input mono"
            value={filename}
            onChange={(e) => setFilename(e.target.value)}
            autoFocus
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="off"
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !loading) handleOk();
            }}
            disabled={loading}
          />
          <div className="field-hint">Renaming triggers a copy + delete on object storage.</div>
        </div>

        <div className="field" style={{ marginBottom: 0 }}>
          <div className="field-label">Location</div>
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
                {directory ? `/${directory}/` : '/ (root)'}
              </span>
            </div>
            <button
              className="btn"
              style={{ height: 26, fontSize: 11.5, padding: '0 10px' }}
              onClick={() => setFolderPickerOpen(true)}
              disabled={loading}
            >
              <SwapOutlined /> Move to…
            </button>
          </div>
        </div>
      </Modal>

      {folderPickerOpen && (
        <FolderPickerModal
          open={true}
          onClose={() => setFolderPickerOpen(false)}
          selectedPath={directory}
          onConfirm={(path: string) => setDirectory(path)}
          title="Move to Folder"
        />
      )}
    </>
  );
}
