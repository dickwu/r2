'use client';

import { useEffect, useCallback, useState, useRef } from 'react';
import { App, Switch } from 'antd';
import {
  UploadOutlined,
  FolderOutlined,
  FileAddOutlined,
  SwapOutlined,
  CheckOutlined,
} from '@ant-design/icons';
import { open } from '@tauri-apps/plugin-dialog';
import { invoke } from '@tauri-apps/api/core';
import {
  useUploadStore,
  selectHasActiveUploads,
  selectHasSuccessfulUploads,
} from '@/app/stores/uploadStore';
import type { StorageConfig } from '@/app/lib/r2cache';
import UploadTaskList from '@/app/components/UploadTaskList';
import FolderPickerModal from '@/app/components/folder/FolderPickerModal';
import { renameKey, type RenameMode } from '@/app/utils/renameKey';
import Modal from '@/app/components/ui/Modal';

// Get content type from file extension
function getContentType(fileName: string): string {
  const ext = fileName.split('.').pop()?.toLowerCase() || '';
  const mimeTypes: Record<string, string> = {
    jpg: 'image/jpeg',
    jpeg: 'image/jpeg',
    png: 'image/png',
    gif: 'image/gif',
    webp: 'image/webp',
    svg: 'image/svg+xml',
    ico: 'image/x-icon',
    mp4: 'video/mp4',
    webm: 'video/webm',
    mov: 'video/quicktime',
    avi: 'video/x-msvideo',
    mkv: 'video/x-matroska',
    mp3: 'audio/mpeg',
    wav: 'audio/wav',
    ogg: 'audio/ogg',
    m4a: 'audio/mp4',
    pdf: 'application/pdf',
    doc: 'application/msword',
    docx: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
    xls: 'application/vnd.ms-excel',
    xlsx: 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
    ppt: 'application/vnd.ms-powerpoint',
    pptx: 'application/vnd.openxmlformats-officedocument.presentationml.presentation',
    txt: 'text/plain',
    html: 'text/html',
    css: 'text/css',
    js: 'text/javascript',
    json: 'application/json',
    xml: 'application/xml',
    csv: 'text/csv',
    md: 'text/markdown',
    zip: 'application/zip',
    tar: 'application/x-tar',
    gz: 'application/gzip',
    rar: 'application/vnd.rar',
    '7z': 'application/x-7z-compressed',
  };
  return mimeTypes[ext] || 'application/octet-stream';
}

interface UploadModalProps {
  open: boolean;
  onClose: () => void;
  currentPath: string;
  config: StorageConfig | null;
  dropQueue: string[][];
  onDropHandled: () => void;
  onUploadComplete: () => void;
  onCredentialsUpdate: () => void;
}

export default function UploadModal({
  open: isOpen,
  onClose,
  currentPath,
  config,
  dropQueue,
  onDropHandled,
  onUploadComplete,
}: UploadModalProps) {
  const { message } = App.useApp();
  const [folderPickerOpen, setFolderPickerOpen] = useState(false);
  const [renameMode, setRenameMode] = useState<RenameMode>('overwrite');
  const [over, setOver] = useState(false);
  const isProcessingDropRef = useRef(false);
  const uploadPath = useUploadStore((s) => s.uploadPath);
  const setUploadPath = useUploadStore((s) => s.setUploadPath);
  const setConfig = useUploadStore((s) => s.setConfig);
  const addTasks = useUploadStore((s) => s.addTasks);
  const existingTasks = useUploadStore((s) => s.tasks);
  const clearAll = useUploadStore((s) => s.clearAll);
  const hasActiveUploads = useUploadStore(selectHasActiveUploads);
  const hasSuccessfulUploads = useUploadStore(selectHasSuccessfulUploads);

  const hasS3Credentials =
    !!config?.accessKeyId &&
    !!config?.secretAccessKey &&
    (config.provider !== 'aws' || !!config.region) &&
    (config.provider !== 'minio' || (!!config.endpointHost && !!config.endpointScheme)) &&
    (config.provider !== 'rustfs' || (!!config.endpointHost && !!config.endpointScheme));

  const addUniqueTasks = useCallback(
    (
      tasks: Array<{ filePath: string; fileName: string; fileSize: number; contentType: string }>
    ) => {
      if (tasks.length === 0) return;
      const existingPaths = new Set(existingTasks.map((task) => task.filePath));
      const seen = new Set<string>();
      const uniqueTasks = tasks.filter((task) => {
        if (existingPaths.has(task.filePath) || seen.has(task.filePath)) return false;
        seen.add(task.filePath);
        return true;
      });
      if (uniqueTasks.length > 0) {
        const tasksWithRename = uniqueTasks.map((task) => {
          const renamed = renameKey(task.fileName, renameMode);
          return { ...task, renamedFileName: renamed !== task.fileName ? renamed : undefined };
        });
        addTasks(tasksWithRename);
      }
    },
    [addTasks, existingTasks, renameMode]
  );

  useEffect(() => {
    setConfig(config);
  }, [config, setConfig]);

  useEffect(() => {
    if (isOpen) {
      setUploadPath(currentPath);
    }
  }, [isOpen, currentPath, setUploadPath]);

  useEffect(() => {
    if (!isOpen || dropQueue.length === 0 || isProcessingDropRef.current) return;

    if (!hasS3Credentials) {
      message.warning('S3 credentials required. Please configure them in Account Settings.');
      onDropHandled();
      return;
    }

    if (hasActiveUploads) {
      message.info('Uploads are currently in progress. Please wait before adding more files.');
      onDropHandled();
      return;
    }

    const droppedPaths = dropQueue[0];
    if (!droppedPaths || droppedPaths.length === 0) {
      onDropHandled();
      return;
    }

    isProcessingDropRef.current = true;

    const processDrop = async () => {
      const tasks: Array<{
        filePath: string;
        fileName: string;
        fileSize: number;
        contentType: string;
      }> = [];

      for (const filePath of droppedPaths) {
        try {
          const folderFiles = await invoke<
            Array<{ file_path: string; relative_path: string; file_size: number }>
          >('get_folder_files', { folderPath: filePath });

          if (folderFiles.length === 0) continue;

          for (const file of folderFiles) {
            tasks.push({
              filePath: file.file_path,
              fileName: file.relative_path,
              fileSize: file.file_size,
              contentType: getContentType(file.relative_path),
            });
          }
        } catch (error) {
          const messageText = error instanceof Error ? error.message : String(error);
          if (messageText.includes('Not a directory')) {
            try {
              const [fileSize, fileName] = await invoke<[number, string]>('get_file_info', {
                filePath,
              });
              tasks.push({ filePath, fileName, fileSize, contentType: getContentType(fileName) });
            } catch (fileError) {
              message.error(`Failed to read dropped file: ${filePath}`);
            }
          } else {
            message.error(`Failed to read dropped folder: ${filePath}`);
          }
        }
      }

      addUniqueTasks(tasks);
    };

    processDrop()
      .catch(() => {
        /* errors already surfaced above */
      })
      .finally(() => {
        isProcessingDropRef.current = false;
        onDropHandled();
      });
  }, [
    addUniqueTasks,
    dropQueue,
    hasActiveUploads,
    hasS3Credentials,
    isOpen,
    message,
    onDropHandled,
  ]);

  const handleSelectFiles = useCallback(async () => {
    if (!hasS3Credentials) {
      message.warning('S3 credentials required. Please configure them in Account Settings.');
      return;
    }
    try {
      const selected = await open({ multiple: true, directory: false });
      if (selected && selected.length > 0) {
        const tasks = await Promise.all(
          selected.map(async (filePath) => {
            const [fileSize, fileName] = await invoke<[number, string]>('get_file_info', {
              filePath,
            });
            return { filePath, fileName, fileSize, contentType: getContentType(fileName) };
          })
        );
        addUniqueTasks(tasks);
      }
    } catch (e) {
      /* dialog cancelled */
    }
  }, [hasS3Credentials, addUniqueTasks, message]);

  const handleSelectFolder = useCallback(async () => {
    if (!hasS3Credentials) {
      message.warning('S3 credentials required. Please configure them in Account Settings.');
      return;
    }
    try {
      const selected = await open({ multiple: false, directory: true });
      if (selected) {
        const folderFiles = await invoke<
          Array<{ file_path: string; relative_path: string; file_size: number }>
        >('get_folder_files', { folderPath: selected });

        if (folderFiles.length > 0) {
          const tasks = folderFiles.map((file) => ({
            filePath: file.file_path,
            fileName: file.relative_path,
            fileSize: file.file_size,
            contentType: getContentType(file.relative_path),
          }));
          addUniqueTasks(tasks);
        }
      }
    } catch (e) {
      /* dialog cancelled */
    }
  }, [hasS3Credentials, addUniqueTasks, message]);

  function handleClose() {
    if (!hasActiveUploads) {
      const shouldReload = hasSuccessfulUploads;
      clearAll();
      onClose();
      if (shouldReload) onUploadComplete();
    }
  }

  const totalTasks = existingTasks.length;
  const doneTasks = existingTasks.filter((t) => t.status === 'success').length;

  const footer = (
    <>
      <span
        className="left"
        style={{ fontSize: 12, color: 'var(--text-muted)', fontFamily: 'var(--font-mono)' }}
      >
        {doneTasks}/{totalTasks} complete
      </span>
      <button className="btn" onClick={handleClose} disabled={hasActiveUploads}>
        Close
      </button>
      <button className="btn btn-primary" onClick={handleClose} disabled={hasActiveUploads}>
        <CheckOutlined /> Done
      </button>
    </>
  );

  return (
    <>
      <Modal
        open={isOpen}
        onClose={handleClose}
        title="Upload files"
        subtitle={`Destination: ${uploadPath ? `/${uploadPath.replace(/\/+$/, '')}/` : '/ (root)'}`}
        icon={<UploadOutlined style={{ fontSize: 18 }} />}
        width={620}
        footer={footer}
      >
        {/* Upload path + rename toggle */}
        <div style={{ marginBottom: 14 }}>
          <div
            style={{
              padding: '8px 12px',
              background: 'var(--bg-sunken)',
              borderRadius: 8,
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'space-between',
              gap: 8,
              marginBottom: 10,
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
                {uploadPath ? `/${uploadPath.replace(/\/+$/, '')}/` : '/ (root)'}
              </span>
            </div>
            <button
              className="btn"
              style={{ height: 26, fontSize: 12, padding: '0 10px' }}
              onClick={() => setFolderPickerOpen(true)}
              disabled={hasActiveUploads}
            >
              <SwapOutlined /> Change...
            </button>
          </div>

          <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <Switch
              size="small"
              checked={renameMode === 'auto-rename'}
              onChange={(checked) => setRenameMode(checked ? 'auto-rename' : 'overwrite')}
              disabled={hasActiveUploads}
            />
            <span style={{ fontSize: 12.5, color: 'var(--text-muted)' }}>
              Auto-rename to avoid collisions
            </span>
          </div>
        </div>

        {!hasS3Credentials && (
          <div
            style={{
              marginBottom: 14,
              padding: '10px 12px',
              background: 'rgba(243,128,32,0.08)',
              borderRadius: 8,
              border: '1px solid rgba(243,128,32,0.25)',
              fontSize: 12.5,
              color: 'var(--text-muted)',
            }}
          >
            S3 credentials required for uploads — configure in Account Settings.
          </div>
        )}

        {/* Drop zone */}
        <div
          className={`drop-zone${over ? 'over' : ''}`}
          onDragOver={(e) => {
            e.preventDefault();
            if (!hasActiveUploads) setOver(true);
          }}
          onDragLeave={() => setOver(false)}
          onDrop={(e) => {
            e.preventDefault();
            setOver(false);
          }}
          onClick={hasActiveUploads ? undefined : handleSelectFiles}
          style={{
            cursor: hasActiveUploads ? 'not-allowed' : 'pointer',
            opacity: hasActiveUploads ? 0.6 : 1,
          }}
        >
          <FileAddOutlined style={{ fontSize: 28, color: 'var(--accent)' }} />
          <div style={{ fontSize: 13, fontWeight: 500, color: 'var(--text)' }}>
            Drop files or folders here
          </div>
          <div style={{ fontSize: 12 }}>
            or <span style={{ color: 'var(--accent)', textDecoration: 'underline' }}>browse</span>
            {' · '}
            <button
              className="btn"
              style={{ height: 24, fontSize: 11.5, padding: '0 8px', display: 'inline-flex' }}
              onClick={(e) => {
                e.stopPropagation();
                handleSelectFolder();
              }}
              disabled={hasActiveUploads}
            >
              <FolderOutlined /> Folder
            </button>
          </div>
        </div>

        {/* Task list */}
        <UploadTaskList />
      </Modal>

      {folderPickerOpen && (
        <FolderPickerModal
          open={true}
          onClose={() => setFolderPickerOpen(false)}
          selectedPath={uploadPath}
          onConfirm={setUploadPath}
          title="Select Upload Folder"
        />
      )}
    </>
  );
}
