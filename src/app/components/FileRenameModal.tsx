'use client';

import { useState, useEffect } from 'react';
import { Modal, Input, Form, App, Button } from 'antd';
import { FolderOutlined, SwapOutlined } from '@ant-design/icons';
import FolderPickerModal from './folder/FolderPickerModal';

export interface FileRenameModalProps {
  open: boolean;
  onClose: () => void;
  fileName: string;
  filePath: string; // Full key/path
  onRename: (newPath: string) => Promise<void>;
}

export default function FileRenameModal({
  open,
  onClose,
  fileName,
  filePath,
  onRename,
}: FileRenameModalProps) {
  const [form] = Form.useForm();
  const [loading, setLoading] = useState(false);
  const [folderPickerOpen, setFolderPickerOpen] = useState(false);
  const [directory, setDirectory] = useState('');
  const { message } = App.useApp();

  // Extract current directory and filename
  const currentDir = filePath.substring(0, filePath.lastIndexOf('/') + 1);
  const currentName = fileName;

  // Directory path without trailing slash for display
  const currentDirDisplay = currentDir ? currentDir.replace(/\/$/, '') : '';

  // Reset form when modal opens
  useEffect(() => {
    if (open) {
      form.setFieldsValue({
        filename: currentName,
      });
      setDirectory(currentDirDisplay);
    }
  }, [open, currentDirDisplay, currentName, form]);

  const handleOk = async () => {
    try {
      const values = await form.validateFields();
      let dir = directory ? directory.trim() : '';
      const filename = values.filename.trim();

      if (!filename) {
        message.error('File name cannot be empty');
        return;
      }

      // Auto-add trailing slash if directory is not empty and doesn't end with /
      if (dir && !dir.endsWith('/')) {
        dir += '/';
      }

      // Build new path
      const newPath = dir ? `${dir}${filename}` : filename;

      // Check if path changed
      if (newPath === filePath) {
        message.info('No changes made');
        onClose();
        return;
      }

      setLoading(true);
      await onRename(newPath);
      message.success('File renamed/moved successfully');
      onClose();
      form.resetFields();
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
      form.resetFields();
      onClose();
    }
  };

  const handleFolderSelect = (path: string) => {
    setDirectory(path);
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
        <Form form={form} layout="vertical">
          {/* File Name - at the top */}
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

      {/* Folder Picker Modal */}
      <FolderPickerModal
        open={folderPickerOpen}
        onClose={() => setFolderPickerOpen(false)}
        selectedPath={directory}
        onConfirm={handleFolderSelect}
        title="Move to Folder"
      />
    </>
  );
}
