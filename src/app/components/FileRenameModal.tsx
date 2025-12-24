'use client';

import { useState, useEffect, useRef } from 'react';
import { Modal, Input, Form, App, Tree, Space } from 'antd';
import { FolderOutlined } from '@ant-design/icons';
import type { DataNode } from 'antd/es/tree';
import type { R2Config } from './ConfigModal';
import { listAllR2ObjectsRecursive } from '../lib/r2api';

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
      {/* <Input value="/" disabled width={20} style={{ fontSize: 12, width: '30px' }}/> */}
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
  const [loadingFolders, setLoadingFolders] = useState(false);
  const [treeData, setTreeData] = useState<DataNode[]>([]);
  const { message } = App.useApp();
  const treeContainerRef = useRef<HTMLDivElement>(null);

  // Extract current directory and filename
  const currentDir = filePath.substring(0, filePath.lastIndexOf('/') + 1);
  const currentName = fileName;

  // Directory path without trailing slash for display
  const currentDirDisplay = currentDir ? currentDir.replace(/\/$/, '') : '';

  // Load folders when modal opens
  useEffect(() => {
    if (open && config) {
      form.setFieldsValue({
        directory: currentDir,
        filename: currentName,
      });
      loadFolders();
    }
  }, [open, currentDir, currentName, form]);

  // Auto-scroll to highlighted directory after tree loads
  useEffect(() => {
    if (treeData.length > 0 && treeContainerRef.current) {
      // Wait for tree to render
      setTimeout(() => {
        const selectedNode = treeContainerRef.current?.querySelector('.ant-tree-node-selected');
        if (selectedNode) {
          selectedNode.scrollIntoView({
            behavior: 'smooth',
            block: 'center',
            inline: 'nearest',
          });
        }
      }, 100);
    }
  }, [treeData]);

  const loadFolders = async () => {
    if (!config) return;

    setLoadingFolders(true);
    try {
      const allObjects = await listAllR2ObjectsRecursive(config);

      // Extract unique folder paths from all object keys
      const folderSet = new Set<string>();

      for (const obj of allObjects) {
        const key = obj.key;
        let currentPath = '';
        const parts = key.split('/');

        // Build all intermediate folder paths
        for (let i = 0; i < parts.length - 1; i++) {
          currentPath += parts[i] + '/';
          folderSet.add(currentPath);
        }
      }

      // Build tree structure
      const tree = buildFolderTree(Array.from(folderSet).sort());
      setTreeData(tree);
    } catch (error) {
      console.error('Failed to load folders:', error);
      // Don't show error to user - they can still type manually
    } finally {
      setLoadingFolders(false);
    }
  };

  // Convert flat folder paths to tree structure
  const buildFolderTree = (folders: string[]): DataNode[] => {
    const root: DataNode[] = [];
    const map = new Map<string, DataNode>();

    // Add root node
    const rootNode: DataNode = {
      title: '/ (root)',
      key: '',
      icon: <FolderOutlined />,
      children: [],
    };
    root.push(rootNode);
    map.set('', rootNode);

    // Sort folders to ensure parents come before children
    const sortedFolders = folders.sort();

    for (const folderPath of sortedFolders) {
      if (!folderPath) continue; // Skip empty root

      const parts = folderPath.replace(/\/$/, '').split('/');
      const folderName = parts[parts.length - 1];
      const parentPath = parts.length > 1 ? parts.slice(0, -1).join('/') + '/' : '';

      const node: DataNode = {
        title: folderName,
        key: folderPath,
        icon: <FolderOutlined />,
        children: [],
      };

      map.set(folderPath, node);

      // Add to parent
      const parent = map.get(parentPath);
      if (parent && parent.children) {
        parent.children.push(node);
      }
    }

    return root;
  };

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
      width={500}
    >
      <Form form={form} layout="vertical">
        <Form.Item label="Directory Path" name="directory" initialValue={currentDirDisplay}>
          <DirectoryInput />
        </Form.Item>

        {/* Folder Tree */}
        {treeData.length > 0 && (
          <Form.Item noStyle shouldUpdate={(prev, curr) => prev.directory !== curr.directory}>
            {() => {
              const currentDirectory = form.getFieldValue('directory') || '';
              // Add trailing slash to match tree keys
              const treeKey =
                currentDirectory && !currentDirectory.endsWith('/')
                  ? currentDirectory + '/'
                  : currentDirectory;

              return (
                <div style={{ marginBottom: 16 }}>
                  <div style={{ marginBottom: 8, fontSize: 14, fontWeight: 500 }}>
                    Existing Folders
                  </div>
                  <div
                    ref={treeContainerRef}
                    style={{
                      border: '1px solid #d9d9d9',
                      borderRadius: 6,
                      padding: 12,
                      maxHeight: 300,
                      overflowY: 'auto',
                    }}
                  >
                    {loadingFolders ? (
                      <div style={{ textAlign: 'center', padding: 20, color: '#999' }}>
                        Loading folders...
                      </div>
                    ) : (
                      <Tree
                        showIcon
                        defaultExpandAll
                        selectedKeys={[treeKey]}
                        treeData={treeData}
                        onSelect={(selectedKeys) => {
                          if (selectedKeys.length > 0) {
                            // Remove trailing slash for form value (we add it back on save)
                            const value = (selectedKeys[0] as string).replace(/\/$/, '');
                            form.setFieldsValue({ directory: value });
                          }
                        }}
                      />
                    )}
                  </div>
                </div>
              );
            }}
          </Form.Item>
        )}
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
