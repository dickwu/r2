'use client';

import { useEffect, useCallback, useState, useRef } from 'react';
import { Modal, Typography, Button, App } from 'antd';
import { FolderOutlined, FileAddOutlined, SwapOutlined } from '@ant-design/icons';
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

const { Text } = Typography;

// Get content type from file extension
function getContentType(fileName: string): string {
  const ext = fileName.split('.').pop()?.toLowerCase() || '';
  const mimeTypes: Record<string, string> = {
    // Images
    jpg: 'image/jpeg',
    jpeg: 'image/jpeg',
    png: 'image/png',
    gif: 'image/gif',
    webp: 'image/webp',
    svg: 'image/svg+xml',
    ico: 'image/x-icon',
    // Video
    mp4: 'video/mp4',
    webm: 'video/webm',
    mov: 'video/quicktime',
    avi: 'video/x-msvideo',
    mkv: 'video/x-matroska',
    // Audio
    mp3: 'audio/mpeg',
    wav: 'audio/wav',
    ogg: 'audio/ogg',
    m4a: 'audio/mp4',
    // Documents
    pdf: 'application/pdf',
    doc: 'application/msword',
    docx: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
    xls: 'application/vnd.ms-excel',
    xlsx: 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
    ppt: 'application/vnd.ms-powerpoint',
    pptx: 'application/vnd.openxmlformats-officedocument.presentationml.presentation',
    // Text
    txt: 'text/plain',
    html: 'text/html',
    css: 'text/css',
    js: 'text/javascript',
    json: 'application/json',
    xml: 'application/xml',
    csv: 'text/csv',
    md: 'text/markdown',
    // Archives
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
    (tasks: Array<{ filePath: string; fileName: string; fileSize: number; contentType: string }>) => {
      if (tasks.length === 0) return;

      const existingPaths = new Set(existingTasks.map((task) => task.filePath));
      const seen = new Set<string>();
      const uniqueTasks = tasks.filter((task) => {
        if (existingPaths.has(task.filePath) || seen.has(task.filePath)) {
          return false;
        }
        seen.add(task.filePath);
        return true;
      });

      if (uniqueTasks.length > 0) {
        addTasks(uniqueTasks);
      }
    },
    [addTasks, existingTasks]
  );

  // Sync config to store
  useEffect(() => {
    setConfig(config);
  }, [config, setConfig]);

  // Reset upload path when modal opens
  useEffect(() => {
    if (isOpen) {
      setUploadPath(currentPath);
    }
  }, [isOpen, currentPath, setUploadPath]);

  useEffect(() => {
    if (!isOpen || dropQueue.length === 0 || isProcessingDropRef.current) {
      return;
    }

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

          if (folderFiles.length === 0) {
            continue;
          }

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
              tasks.push({
                filePath,
                fileName,
                fileSize,
                contentType: getContentType(fileName),
              });
            } catch (fileError) {
              console.error('Failed to read dropped file:', fileError);
              message.error(`Failed to read dropped file: ${filePath}`);
            }
          } else {
            console.error('Failed to read dropped folder:', error);
            message.error(`Failed to read dropped folder: ${filePath}`);
          }
        }
      }

      addUniqueTasks(tasks);
    };

    processDrop()
      .catch((error) => {
        console.error('Failed to process dropped items:', error);
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
      const selected = await open({
        multiple: true,
        directory: false,
      });

      if (selected && selected.length > 0) {
        // Get file info for each selected file
        const tasks = await Promise.all(
          selected.map(async (filePath) => {
            const [fileSize, fileName] = await invoke<[number, string]>('get_file_info', {
              filePath,
            });
            return {
              filePath,
              fileName,
              fileSize,
              contentType: getContentType(fileName),
            };
          })
        );
        addUniqueTasks(tasks);
      }
    } catch (e) {
      console.error('Failed to select files:', e);
    }
  }, [hasS3Credentials, addUniqueTasks, message]);

  const handleSelectFolder = useCallback(async () => {
    if (!hasS3Credentials) {
      message.warning('S3 credentials required. Please configure them in Account Settings.');
      return;
    }

    try {
      const selected = await open({
        multiple: false,
        directory: true,
      });

      if (selected) {
        // Get all files in the folder recursively
        const folderFiles = await invoke<
          Array<{ file_path: string; relative_path: string; file_size: number }>
        >('get_folder_files', { folderPath: selected });

        if (folderFiles.length > 0) {
          const tasks = folderFiles.map((file) => ({
            filePath: file.file_path,
            fileName: file.relative_path, // Use relative path as the "name" to preserve folder structure
            fileSize: file.file_size,
            contentType: getContentType(file.relative_path),
          }));
          addUniqueTasks(tasks);
        }
      }
    } catch (e) {
      console.error('Failed to select folder:', e);
    }
  }, [hasS3Credentials, addUniqueTasks, message]);

  function handleClose() {
    if (!hasActiveUploads) {
      const shouldReload = hasSuccessfulUploads;
      clearAll();
      onClose();
      if (shouldReload) {
        onUploadComplete();
      }
    }
  }

  return (
    <>
      <Modal
        open={isOpen}
        onCancel={handleClose}
        footer={null}
        title="Upload Files"
        width={520}
        centered
        destroyOnHidden
        maskClosable={!hasActiveUploads}
        closable={!hasActiveUploads}
      >
        <div style={{ marginBottom: 16 }}>
          <Text type="secondary" style={{ display: 'block', marginBottom: 4 }}>
            Upload to:
          </Text>
          <div
            style={{
              padding: '8px 12px',
              background: 'var(--bg-secondary)',
              borderRadius: 6,
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'space-between',
              gap: 8,
            }}
          >
            <div style={{ display: 'flex', alignItems: 'center', gap: 8, flex: 1, minWidth: 0 }}>
              <FolderOutlined style={{ color: 'var(--text-secondary)', flexShrink: 0 }} />
              <span
                style={{
                  fontFamily: 'monospace',
                  overflow: 'hidden',
                  textOverflow: 'ellipsis',
                  whiteSpace: 'nowrap',
                }}
              >
                {uploadPath ? `/${uploadPath}/` : '/ (root)'}
              </span>
            </div>
            <Button
              size="small"
              icon={<SwapOutlined />}
              onClick={() => setFolderPickerOpen(true)}
              disabled={hasActiveUploads}
            >
              Change...
            </Button>
          </div>
        </div>

        {!hasS3Credentials && (
          <div
            style={{
              marginBottom: 16,
              padding: '12px',
              background: '#fff7e6',
              borderRadius: 6,
              border: '1px solid #ffd591',
              display: 'flex',
              justifyContent: 'space-between',
              alignItems: 'center',
            }}
          >
            <Text type="warning">S3 credentials required for uploads</Text>
            <Text type="secondary" style={{ fontSize: 12 }}>
              Configure in Account Settings
            </Text>
          </div>
        )}

        <div style={{ display: 'flex', gap: 12 }}>
          <div
            onClick={hasActiveUploads ? undefined : handleSelectFiles}
            style={{
              flex: 1,
              border: '2px dashed #d9d9d9',
              borderRadius: 8,
              padding: '24px 16px',
              textAlign: 'center',
              cursor: hasActiveUploads ? 'not-allowed' : 'pointer',
              opacity: hasActiveUploads ? 0.5 : 1,
              transition: 'border-color 0.3s',
            }}
            onMouseEnter={(e) => {
              if (!hasActiveUploads) {
                e.currentTarget.style.borderColor = '#f6821f';
              }
            }}
            onMouseLeave={(e) => {
              e.currentTarget.style.borderColor = '#d9d9d9';
            }}
          >
            <p style={{ marginBottom: 8 }}>
              <FileAddOutlined style={{ color: '#f6821f', fontSize: 36 }} />
            </p>
            <p style={{ fontSize: 14, marginBottom: 4 }}>Select Files</p>
            <p style={{ color: '#999', fontSize: 12 }}>Single or multiple</p>
          </div>

          <div
            onClick={hasActiveUploads ? undefined : handleSelectFolder}
            style={{
              flex: 1,
              border: '2px dashed #d9d9d9',
              borderRadius: 8,
              padding: '24px 16px',
              textAlign: 'center',
              cursor: hasActiveUploads ? 'not-allowed' : 'pointer',
              opacity: hasActiveUploads ? 0.5 : 1,
              transition: 'border-color 0.3s',
            }}
            onMouseEnter={(e) => {
              if (!hasActiveUploads) {
                e.currentTarget.style.borderColor = '#f6821f';
              }
            }}
            onMouseLeave={(e) => {
              e.currentTarget.style.borderColor = '#d9d9d9';
            }}
          >
            <p style={{ marginBottom: 8 }}>
              <FolderOutlined style={{ color: '#f6821f', fontSize: 36 }} />
            </p>
            <p style={{ fontSize: 14, marginBottom: 4 }}>Select Folder</p>
            <p style={{ color: '#999', fontSize: 12 }}>Upload entire folder</p>
          </div>
        </div>

        <UploadTaskList />
      </Modal>

      {/* Folder Picker Modal */}
      <FolderPickerModal
        open={folderPickerOpen}
        onClose={() => setFolderPickerOpen(false)}
        selectedPath={uploadPath}
        onConfirm={setUploadPath}
        title="Select Upload Folder"
      />
    </>
  );
}
