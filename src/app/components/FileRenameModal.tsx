'use client';

import { useState } from 'react';
import { Modal, Input, Form, App, Button } from 'antd';
import { FolderOutlined, SwapOutlined } from '@ant-design/icons';
import FolderPickerModal from '@/app/components/folder/FolderPickerModal';
import { renameObject, StorageConfig } from '@/app/lib/r2cache';
import type { FileItem } from '@/app/hooks/useR2Files';

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
  const [form] = Form.useForm();
  const [loading, setLoading] = useState(false);
  const [folderPickerOpen, setFolderPickerOpen] = useState(false);
  const { message } = App.useApp();

  const currentDir = file.key.substring(0, file.key.lastIndexOf('/') + 1);
  const currentDirDisplay = currentDir ? currentDir.replace(/\/$/, '') : '';
  const [directory, setDirectory] = useState(currentDirDisplay);

  const handleOk = async () => {
    try {
      const values = await form.validateFields();
      let dir = directory ? directory.trim() : '';
      const filename = values.filename.trim();

      if (!filename) {
        message.error('File name cannot be empty');
        return;
      }

      if (dir && !dir.endsWith('/')) {
        dir += '/';
      }

      const newPath = dir ? `${dir}${filename}` : filename;

      if (newPath === file.key) {
        message.info('No changes made');
        onClose();
        return;
      }

      setLoading(true);
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

  const handleCancel = () => {
    if (!loading) {
      onClose();
    }
  };

  return (
    <>
      <Modal
        title="Rename / Move File"
        open={open}
        onOk={handleOk}
        onCancel={handleCancel}
        confirmLoading={loading}
        okText="Save"
        cancelText="Cancel"
        width={480}
        centered
      >
        <Form form={form} layout="vertical" initialValues={{ filename: file.name }}>
          <Form.Item
            label="File Name"
            name="filename"
            rules={[
              { required: true, message: 'Please enter a file name' },
              {
                validator: (_, value) => {
                  if (value && value.includes('/')) {
                    return Promise.reject(new Error('File name cannot contain /'));
                  }
                  return Promise.resolve();
                },
              },
            ]}
          >
            <Input placeholder="e.g., document.pdf" />
          </Form.Item>

          {/* Current Path - read-only display */}
          <Form.Item label="Location">
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
                  {directory ? `/${directory}/` : '/ (root)'}
                </span>
              </div>
              <Button
                size="small"
                icon={<SwapOutlined />}
                onClick={() => setFolderPickerOpen(true)}
                disabled={loading}
              >
                Move to...
              </Button>
            </div>
          </Form.Item>
        </Form>
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
