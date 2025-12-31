'use client';

import { useState, useEffect } from 'react';
import { Modal, Input, Form, App, Space } from 'antd';
import { FolderOutlined } from '@ant-design/icons';
import type { R2Config } from './ConfigModal';
import FolderTreePicker from './FolderTreePicker';

// Custom input component for directory path with visual separators
interface DirectoryInputProps {
  value?: string;
  onChange?: (value: string) => void;
  placeholder?: string;
}

function DirectoryInput({ value = '', onChange, placeholder }: DirectoryInputProps) {
  return (
    <Space.Compact style={{ width: '100%' }}>
      <Input prefix={<FolderOutlined />} disabled style={{ fontSize: 12, width: '34px' }} />
      <Input
        value={value}
        onChange={(e) => onChange?.(e.target.value)}
        placeholder={placeholder || 'empty for root'}
        allowClear
        style={{ flex: 24 }}
      />
    </Space.Compact>
  );
}

export interface FileRenameModalProps {
  open: boolean;
  onClose: () => void;
  fileName: string;
  filePath: string; // Full key/path
  onRename: (newPath: string) => Promise<void>;
  config?: R2Config | null;
}

export default function FileRenameModal({
  open,
  onClose,
  fileName,
  filePath,
  onRename,
  config,
}: FileRenameModalProps) {
  const [form] = Form.useForm();
  const [loading, setLoading] = useState(false);
  const { message } = App.useApp();

  // Extract current directory and filename
  const currentDir = filePath.substring(0, filePath.lastIndexOf('/') + 1);
  const currentName = fileName;

  // Directory path without trailing slash for display
  const currentDirDisplay = currentDir ? currentDir.replace(/\/$/, '') : '';

  // Reset form when modal opens
  useEffect(() => {
    if (open && config) {
      form.setFieldsValue({
        directory: currentDirDisplay,
        filename: currentName,
      });
    }
  }, [open, currentDirDisplay, currentName, form, config]);

  const handleOk = async () => {
    try {
      const values = await form.validateFields();
      let directory = values.directory ? values.directory.trim() : '';
      const filename = values.filename.trim();

      if (!filename) {
        message.error('File name cannot be empty');
        return;
      }

      // Auto-add trailing slash if directory is not empty and doesn't end with /
      if (directory && !directory.endsWith('/')) {
        directory += '/';
      }

      // Build new path
      const newPath = directory ? `${directory}${filename}` : filename;

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

  return (
    <Modal
      title="Rename/Move File"
      open={open}
      onOk={handleOk}
      onCancel={handleCancel}
      confirmLoading={loading}
      okText="Save"
      cancelText="Cancel"
      width={'90%'}
      height={'90%'}
      style={{ top: '3%' }}
    >
      <Form form={form} layout="vertical" style={{ height: '80vh', overflow: 'auto' }}>
        <Form.Item label="Directory Path" name="directory" initialValue={currentDirDisplay}>
          <DirectoryInput />
        </Form.Item>

        {/* Folder Tree Picker */}
        <Form.Item noStyle shouldUpdate={(prev, curr) => prev.directory !== curr.directory}>
          {() => {
            const currentDirectory = form.getFieldValue('directory') || '';
            return (
              <FolderTreePicker
                selectedPath={currentDirectory}
                onSelect={(path) => form.setFieldsValue({ directory: path })}
              />
            );
          }}
        </Form.Item>

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
      </Form>
    </Modal>
  );
}
