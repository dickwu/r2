'use client';

import { useEffect, useCallback, useState } from 'react';
import { Modal, Typography, Button, App } from 'antd';
import { FolderOutlined, FileAddOutlined, SwapOutlined } from '@ant-design/icons';
import { open } from '@tauri-apps/plugin-dialog';
import { invoke } from '@tauri-apps/api/core';
import {
  useUploadStore,
  selectHasActiveUploads,
  selectHasSuccessfulUploads,
} from '../stores/uploadStore';
import type { StorageConfig } from '../lib/r2cache';
import UploadTaskList from './UploadTaskList';
import FolderPickerModal from './folder/FolderPickerModal';

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
  onUploadComplete: () => void;
  onCredentialsUpdate: () => void;
}

export default function UploadModal({
  open: isOpen,
  onClose,
  currentPath,
  config,
  onUploadComplete,
}: UploadModalProps) {
  const { message } = App.useApp();
  const [folderPickerOpen, setFolderPickerOpen] = useState(false);
  const uploadPath = useUploadStore((s) => s.uploadPath);
  const setUploadPath = useUploadStore((s) => s.setUploadPath);
  const setConfig = useUploadStore((s) => s.setConfig);
  const addTasks = useUploadStore((s) => s.addTasks);
  const clearAll = useUploadStore((s) => s.clearAll);
  const hasActiveUploads = useUploadStore(selectHasActiveUploads);
  const hasSuccessfulUploads = useUploadStore(selectHasSuccessfulUploads);

  const hasS3Credentials =
    !!config?.accessKeyId &&
    !!config?.secretAccessKey &&
    (config.provider !== 'aws' || !!config.region) &&
    (config.provider !== 'minio' || (!!config.endpointHost && !!config.endpointScheme)) &&
    (config.provider !== 'rustfs' || (!!config.endpointHost && !!config.endpointScheme));

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
        addTasks(tasks);
      }
    } catch (e) {
      console.error('Failed to select files:', e);
    }
  }, [hasS3Credentials, addTasks, message]);

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
          addTasks(tasks);
        }
      }
    } catch (e) {
      console.error('Failed to select folder:', e);
    }
  }, [hasS3Credentials, addTasks, message]);

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
