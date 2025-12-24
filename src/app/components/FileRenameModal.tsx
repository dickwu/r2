'use client';

import { useState, useEffect, useRef } from 'react';
import { Modal, Input, Form, App, Tree, Space, Tag, Switch } from 'antd';
import { FolderOutlined, InfoCircleOutlined } from '@ant-design/icons';
import type { DataNode } from 'antd/es/tree';
import type { R2Config } from './ConfigModal';
import { getAllDirectoryNodes } from '../lib/indexeddb';
import { formatBytes } from '../utils/formatBytes';

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
  const [showMetadata, setShowMetadata] = useState(true);
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

  // Rebuild tree when metadata visibility changes
  useEffect(() => {
    if (treeData.length > 0) {
      loadFolders();
    }
  }, [showMetadata]);

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
      // Load directory nodes from IndexedDB (pre-built during sync)
      const directoryNodes = await getAllDirectoryNodes();
      // Extract folder paths from directory nodes
      const folderPaths = directoryNodes.map((node) => node.path).sort();
      // Build tree structure with metadata
      const tree = buildFolderTree(folderPaths, directoryNodes);
      setTreeData(tree);
    } catch (error) {
      console.error('Failed to load folders:', error);
      // Don't show error to user - they can still type manually
    } finally {
      setLoadingFolders(false);
    }
  };

  // Convert flat folder paths to tree structure with metadata
  const buildFolderTree = (
    folders: string[],
    directoryNodes: Array<{ path: string; totalFileCount: number; totalSize: number }>
  ): DataNode[] => {
    const root: DataNode[] = [];
    const map = new Map<string, DataNode>();

    // Create a lookup map for directory metadata
    const metadataMap = new Map(directoryNodes.map((node) => [node.path, node]));

    // Get root metadata
    const rootMetadata = metadataMap.get('');
    const rootTitle =
      rootMetadata && showMetadata ? (
        <>
          <span style={{ flex: 1 }}>/ (root)</span>
          <Tag color="blue" style={{ fontSize: 11, marginLeft: 'auto' }}>
            {rootMetadata.totalFileCount} files · {formatBytes(rootMetadata.totalSize)}
          </Tag>
        </>
      ) : (
        '/ (root)'
      );

    // Add root node
    const rootNode: DataNode = {
      title: rootTitle,
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

      // Normalize path: ensure trailing slash for consistency
      const normalizedPath = folderPath.endsWith('/') ? folderPath : folderPath + '/';

      const parts = folderPath.replace(/\/$/, '').split('/');
      const folderName = parts[parts.length - 1];
      const parentPath = parts.length > 1 ? parts.slice(0, -1).join('/') + '/' : '';

      // Get metadata for this folder (try both with and without trailing slash)
      const metadata = metadataMap.get(folderPath) || metadataMap.get(normalizedPath);
      const title =
        metadata && showMetadata ? (
          <>
            <span style={{ flex: 1 }}>{folderName}</span>
            <Tag color="blue" style={{ fontSize: 11, marginLeft: 'auto' }}>
              {metadata.totalFileCount} files · {formatBytes(metadata.totalSize)}
            </Tag>
          </>
        ) : (
          folderName
        );

      const node: DataNode = {
        title,
        key: normalizedPath,
        icon: <FolderOutlined />,
        children: [],
      };

      map.set(normalizedPath, node);

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
      width={'90%'}
      height={'90%'}
      style={{ top: '3%' }}
    >
      <Form form={form} layout="vertical" style={{ height: '80vh', overflow: 'auto' }}>
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
                  <div
                    style={{
                      marginBottom: 8,
                      display: 'flex',
                      justifyContent: 'space-between',
                      alignItems: 'center',
                    }}
                  >
                    <span style={{ fontSize: 14, fontWeight: 500 }}>Existing Folders</span>
                    <Space size="small">
                      <InfoCircleOutlined style={{ color: '#999', fontSize: 12 }} />
                      <span style={{ fontSize: 12, color: '#666' }}>Show details</span>
                      <Switch size="small" checked={showMetadata} onChange={setShowMetadata} />
                    </Space>
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
                        style={{
                          fontSize: 13,
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
