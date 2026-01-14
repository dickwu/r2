'use client';

import { useState, useEffect, useRef, useMemo } from 'react';
import { Input, Tree, Space, Tag, Switch } from 'antd';
import {
  FolderOutlined,
  FolderAddOutlined,
  InfoCircleOutlined,
  SearchOutlined,
} from '@ant-design/icons';
import type { DataNode } from 'antd/es/tree';
import { getAllDirectoryNodes } from '../../lib/r2cache';
import { formatBytes } from '../../utils/formatBytes';

interface DirectoryNode {
  path: string;
  totalFileCount: number;
  totalSize: number;
}

export interface FolderTreePickerProps {
  selectedPath: string;
  onSelect: (path: string) => void;
}

export default function FolderTreePicker({ selectedPath, onSelect }: FolderTreePickerProps) {
  const [loading, setLoading] = useState(false);
  const [directoryNodes, setDirectoryNodes] = useState<DirectoryNode[]>([]);
  const [showMetadata, setShowMetadata] = useState(true);
  const [searchValue, setSearchValue] = useState('');
  const [customPath, setCustomPath] = useState('');
  const treeContainerRef = useRef<HTMLDivElement>(null);

  // Check if current path is a custom (non-existing) path
  const existingPaths = useMemo(() => {
    const paths = new Set<string>();
    paths.add(''); // root
    for (const node of directoryNodes) {
      const normalized = node.path.endsWith('/') ? node.path.slice(0, -1) : node.path;
      paths.add(normalized);
    }
    return paths;
  }, [directoryNodes]);

  const isCustomPath = customPath && !existingPaths.has(customPath.replace(/\/$/, ''));

  // Load folders on mount
  useEffect(() => {
    loadFolders();
  }, []);

  // Auto-scroll to selected node after tree loads
  useEffect(() => {
    if (directoryNodes.length > 0 && treeContainerRef.current) {
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
  }, [directoryNodes]);

  const loadFolders = async () => {
    setLoading(true);
    try {
      const nodes = await getAllDirectoryNodes();
      setDirectoryNodes(nodes);
    } catch (error) {
      console.error('Failed to load folders:', error);
    } finally {
      setLoading(false);
    }
  };

  // Build tree data with search filtering
  const treeData = useMemo(() => {
    if (directoryNodes.length === 0) return [];

    const metadataMap = new Map(directoryNodes.map((node) => [node.path, node]));
    const folderPaths = directoryNodes.map((node) => node.path).sort();

    // Filter paths by search
    const searchLower = searchValue.toLowerCase();
    const matchingPaths = searchValue
      ? folderPaths.filter((path) => path.toLowerCase().includes(searchLower))
      : folderPaths;

    // When searching, also include parent paths to maintain tree structure
    const pathsToInclude = new Set<string>();
    for (const path of matchingPaths) {
      pathsToInclude.add(path);
      // Add all parent paths
      const parts = path.replace(/\/$/, '').split('/');
      for (let i = 1; i < parts.length; i++) {
        pathsToInclude.add(parts.slice(0, i).join('/') + '/');
      }
    }
    // Always include root
    pathsToInclude.add('');

    const root: DataNode[] = [];
    const map = new Map<string, DataNode>();

    // Root node
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

    const rootNode: DataNode = {
      title: rootTitle,
      key: '',
      icon: <FolderOutlined />,
      children: [],
    };
    root.push(rootNode);
    map.set('', rootNode);

    // Build filtered tree
    const sortedFolders = Array.from(pathsToInclude).sort();

    for (const folderPath of sortedFolders) {
      if (!folderPath) continue;

      const normalizedPath = folderPath.endsWith('/') ? folderPath : folderPath + '/';
      const parts = folderPath.replace(/\/$/, '').split('/');
      const folderName = parts[parts.length - 1];
      const parentPath = parts.length > 1 ? parts.slice(0, -1).join('/') + '/' : '';

      const metadata = metadataMap.get(folderPath) || metadataMap.get(normalizedPath);
      const isMatch = searchValue && folderPath.toLowerCase().includes(searchLower);

      const title =
        metadata && showMetadata ? (
          <>
            <span style={{ flex: 1, fontWeight: isMatch ? 600 : 400 }}>{folderName}</span>
            <Tag color="blue" style={{ fontSize: 11, marginLeft: 'auto' }}>
              {metadata.totalFileCount} files · {formatBytes(metadata.totalSize)}
            </Tag>
          </>
        ) : (
          <span style={{ fontWeight: isMatch ? 600 : 400 }}>{folderName}</span>
        );

      const node: DataNode = {
        title,
        key: normalizedPath,
        icon: <FolderOutlined />,
        children: [],
      };

      map.set(normalizedPath, node);

      const parent = map.get(parentPath);
      if (parent && parent.children) {
        parent.children.push(node);
      }
    }

    return root;
  }, [directoryNodes, showMetadata, searchValue]);

  // Selected key for tree
  const selectedKey =
    selectedPath && !selectedPath.endsWith('/') ? selectedPath + '/' : selectedPath;

  // Handle custom path input
  const handleCustomPathChange = (value: string) => {
    // Normalize: remove leading slash, allow trailing slash
    let normalized = value.replace(/^\/+/, '');
    setCustomPath(normalized);
    onSelect(normalized);
  };

  // When selecting from tree, clear custom path input
  const handleTreeSelect = (path: string) => {
    setCustomPath('');
    onSelect(path);
  };

  return (
    <div style={{ marginBottom: 16 }}>
      {/* Custom Path Input */}
      <div style={{ marginBottom: 12 }}>
        <div style={{ marginBottom: 6, display: 'flex', alignItems: 'center', gap: 6 }}>
          <FolderAddOutlined style={{ color: '#1890ff' }} />
          <span style={{ fontSize: 14, fontWeight: 500 }}>Custom Path</span>
          {isCustomPath && (
            <Tag color="green" style={{ fontSize: 11, marginLeft: 4 }}>
              New folder
            </Tag>
          )}
        </div>
        <Input
          placeholder="Enter custom path (e.g., photos/2024/january)"
          value={customPath || selectedPath}
          onChange={(e) => handleCustomPathChange(e.target.value)}
          allowClear
          autoComplete="off"
          autoCorrect="off"
          autoCapitalize="off"
          onClear={() => {
            setCustomPath('');
            onSelect('');
          }}
        />
        <div style={{ fontSize: 11, color: '#999', marginTop: 4 }}>
          Type a path to create new folders, or select from existing folders below
        </div>
      </div>

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

      {/* Search Input */}
      <Input
        placeholder="Search folders..."
        prefix={<SearchOutlined style={{ color: '#999' }} />}
        value={searchValue}
        onChange={(e) => setSearchValue(e.target.value)}
        allowClear
        autoComplete="off"
        autoCorrect="off"
        autoCapitalize="off"
        style={{ marginBottom: 8 }}
      />

      <div
        ref={treeContainerRef}
        style={{
          border: '1px solid #d9d9d9',
          borderRadius: 6,
          padding: 12,
        }}
      >
        {loading ? (
          <div style={{ textAlign: 'center', padding: 20, color: '#999' }}>Loading folders...</div>
        ) : treeData.length === 0 ? (
          <div style={{ textAlign: 'center', padding: 20, color: '#999' }}>No folders found</div>
        ) : (
          <Tree
            showIcon
            defaultExpandAll
            selectedKeys={[selectedKey]}
            treeData={treeData}
            virtual
            height={250}
            onSelect={(selectedKeys) => {
              if (selectedKeys.length > 0) {
                const value = (selectedKeys[0] as string).replace(/\/$/, '');
                handleTreeSelect(value);
              }
            }}
            style={{ fontSize: 13 }}
          />
        )}
      </div>
    </div>
  );
}
