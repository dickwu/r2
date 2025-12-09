'use client';

import { useEffect, useState, useCallback, useMemo } from 'react';
import { Store } from '@tauri-apps/plugin-store';
import { getVersion } from '@tauri-apps/api/app';
import { check } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';
import {
  Button,
  Breadcrumb,
  Space,
  App,
  Spin,
  Empty,
  Segmented,
  Badge,
  Tooltip,
  Input,
  Popconfirm,
  Select,
} from 'antd';
import {
  SettingOutlined,
  ReloadOutlined,
  FolderOutlined,
  FileOutlined,
  UploadOutlined,
  HomeOutlined,
  SunOutlined,
  MoonOutlined,
  AppstoreOutlined,
  BarsOutlined,
  CloudSyncOutlined,
  SearchOutlined,
  CaretUpOutlined,
  CaretDownOutlined,
  DeleteOutlined,
  DatabaseOutlined,
} from '@ant-design/icons';
import { useTheme } from './providers';
import ConfigModal, { R2Config } from './components/ConfigModal';
import UploadModal from './components/UploadModal';
import FilePreviewModal from './components/FilePreviewModal';
import FileGridView from './components/FileGridView';
import { useR2Files, FileItem } from './hooks/useR2Files';
import { useFilesSync } from './hooks/useFilesSync';
import { deleteR2Object } from './lib/r2api';
import { useFolderSizeStore } from './stores/folderSizeStore';

function formatBytes(bytes: number): string {
  if (bytes === 0) return '0 B';
  const k = 1024;
  const sizes = ['B', 'KB', 'MB', 'GB'];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + ' ' + sizes[i];
}

function formatDate(date: string): string {
  return new Date(date).toLocaleDateString(undefined, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
  });
}

type ViewMode = 'list' | 'grid';
type SortOrder = 'asc' | 'desc' | null;

export default function Home() {
  const [config, setConfig] = useState<R2Config | null>(null);
  const [loading, setLoading] = useState(true);
  const [configModalOpen, setConfigModalOpen] = useState(false);
  const [uploadModalOpen, setUploadModalOpen] = useState(false);
  const [previewFile, setPreviewFile] = useState<FileItem | null>(null);
  const [currentPath, setCurrentPath] = useState('');
  const [viewMode, setViewMode] = useState<ViewMode>('list');
  const [searchQuery, setSearchQuery] = useState('');
  const [sizeSort, setSizeSort] = useState<SortOrder>(null);
  const [appVersion, setAppVersion] = useState<string>('');
  const [updateAvailable, setUpdateAvailable] = useState<{ version: string; body?: string } | null>(
    null
  );
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const { message, modal } = App.useApp();
  const { theme, toggleTheme } = useTheme();

  const { items, isLoading, isFetching, error, refresh } = useR2Files(config, currentPath);
  const { isSynced, refresh: refreshSync } = useFilesSync(config);

  // Zustand store for folder sizes
  const folderSizes = useFolderSizeStore((state) => state.sizes);
  const calculateSizes = useFolderSizeStore((state) => state.calculateSizes);

  // Filter and sort items
  const filteredItems = useMemo(() => {
    let result = items;

    // Filter by search query
    if (searchQuery.trim()) {
      const query = searchQuery.toLowerCase();
      result = result.filter((item) => item.name.toLowerCase().includes(query));
    }

    // Sort by size (list view only)
    if (sizeSort) {
      result = [...result].sort((a, b) => {
        const sizeA = a.isFolder
          ? typeof folderSizes[a.key] === 'number'
            ? (folderSizes[a.key] as number)
            : 0
          : a.size || 0;
        const sizeB = b.isFolder
          ? typeof folderSizes[b.key] === 'number'
            ? (folderSizes[b.key] as number)
            : 0
          : b.size || 0;
        return sizeSort === 'asc' ? sizeA - sizeB : sizeB - sizeA;
      });
    }

    return result;
  }, [items, searchQuery, sizeSort, folderSizes]);

  // Calculate folder sizes from IndexedDB when items change and sync is complete
  useEffect(() => {
    if (!isSynced || items.length === 0) return;

    const folderKeys = items.filter((item) => item.isFolder).map((item) => item.key);
    if (folderKeys.length > 0) {
      calculateSizes(folderKeys);
    }
  }, [isSynced, items, calculateSizes]);

  // Show error if API fails
  useEffect(() => {
    if (error) {
      console.error('R2 fetch error:', error);
      message.error(`Failed to load files: ${error.message}`);
    }
  }, [error, message]);

  useEffect(() => {
    loadConfig();
    loadVersion();
  }, []);

  async function loadVersion() {
    try {
      const version = await getVersion();
      setAppVersion(version);
    } catch (e) {
      console.error('Failed to get version:', e);
    }
  }

  const checkForUpdate = useCallback(async () => {
    setCheckingUpdate(true);
    try {
      const update = await check();
      if (update) {
        setUpdateAvailable({ version: update.version, body: update.body ?? undefined });
        modal.confirm({
          title: `Update Available: v${update.version}`,
          content: update.body || 'A new version is available. Would you like to update now?',
          okText: 'Update Now',
          cancelText: 'Later',
          onOk: async () => {
            message.loading({ content: 'Downloading update...', key: 'update', duration: 0 });
            await update.downloadAndInstall();
            message.success({ content: 'Update installed! Restarting...', key: 'update' });
            await relaunch();
          },
        });
      } else {
        message.success("You're on the latest version!");
        setUpdateAvailable(null);
      }
    } catch (e) {
      console.error('Update check failed:', e);
      message.error('Failed to check for updates');
    } finally {
      setCheckingUpdate(false);
    }
  }, [message, modal]);

  async function loadConfig() {
    try {
      const store = await Store.load('r2-config.json');
      const savedConfig = await store.get<R2Config>('config');
      console.log('Loaded config:', savedConfig);

      if (!savedConfig || !savedConfig.accountId || !savedConfig.token || !savedConfig.bucket) {
        console.log('Config invalid, opening modal');
        setConfigModalOpen(true);
        setLoading(false);
        return;
      }

      setConfig(savedConfig);
    } catch (e) {
      console.error('Failed to load config:', e);
      setConfigModalOpen(true);
    } finally {
      setLoading(false);
    }
  }

  const handleItemClick = useCallback((item: FileItem) => {
    if (item.isFolder) {
      setCurrentPath(item.key);
      setSearchQuery('');
    } else {
      setPreviewFile(item);
    }
  }, []);

  const handleRefresh = useCallback(() => {
    Promise.all([refresh(), refreshSync()])
      .then(() => {
        message.success('Files refreshed');
      })
      .catch(() => {
        message.error('Failed to refresh files');
      });
  }, [refresh, refreshSync, message]);

  const handleDelete = useCallback(
    async (item: FileItem) => {
      if (!config) return;
      try {
        await deleteR2Object(config, item.key);
        message.success(`Deleted "${item.name}"`);
        // Refresh file list after deletion
        await Promise.all([refresh(), refreshSync()]);
      } catch (e) {
        console.error('Delete error:', e);
        message.error(`Failed to delete: ${e instanceof Error ? e.message : 'Unknown error'}`);
      }
    },
    [config, message, refresh, refreshSync]
  );

  function navigateToPath(path: string) {
    setCurrentPath(path);
    setSearchQuery('');
  }

  async function handleBucketChange(newBucket: string) {
    if (!config || newBucket === config.bucket) return;

    // Get publicDomain for the new bucket
    const bucketConfig = config.buckets?.find((b) => b.name === newBucket);
    const newConfig = {
      ...config,
      bucket: newBucket,
      publicDomain: bucketConfig?.publicDomain,
    };
    setConfig(newConfig);
    setCurrentPath(''); // Reset to root when switching buckets
    setSearchQuery('');

    // Save the new bucket selection
    try {
      const store = await Store.load('r2-config.json');
      await store.set('config', newConfig);
      await store.save();
      message.success(`Switched to bucket: ${newBucket}`);
    } catch (e) {
      console.error('Failed to save bucket change:', e);
    }
  }

  function toggleSizeSort() {
    setSizeSort((prev) => {
      if (prev === null) return 'desc';
      if (prev === 'desc') return 'asc';
      return null;
    });
  }

  // Build breadcrumb items
  const pathParts = currentPath ? currentPath.replace(/\/$/, '').split('/') : [];
  const breadcrumbItems = [
    {
      title: (
        <a onClick={() => navigateToPath('')}>
          <HomeOutlined /> {config?.bucket || 'Root'}
        </a>
      ),
    },
    ...pathParts.map((part, index) => {
      const fullPath = pathParts.slice(0, index + 1).join('/') + '/';
      return {
        title: <a onClick={() => navigateToPath(fullPath)}>{part}</a>,
      };
    }),
  ];

  if (loading) {
    return (
      <div className="center-container">
        <Spin fullscreen />
      </div>
    );
  }

  return (
    <div className="file-manager">
      {/* Toolbar */}
      <div className="toolbar">
        <Space>
          {config?.buckets && config.buckets.length > 1 && (
            <Select
              value={config.bucket}
              onChange={handleBucketChange}
              style={{ minWidth: 140 }}
              suffixIcon={<DatabaseOutlined />}
              options={config.buckets.map((b) => ({ label: b.name, value: b.name }))}
            />
          )}
          <Breadcrumb items={breadcrumbItems} />
        </Space>
        <Space>
          <Input
            placeholder="Search files..."
            prefix={<SearchOutlined />}
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            allowClear
            style={{ width: 200 }}
          />
          <Segmented
            value={viewMode}
            onChange={(value) => setViewMode(value as ViewMode)}
            options={[
              { value: 'list', icon: <BarsOutlined /> },
              { value: 'grid', icon: <AppstoreOutlined /> },
            ]}
          />
          <Button type="primary" icon={<UploadOutlined />} onClick={() => setUploadModalOpen(true)}>
            Upload
          </Button>
          <Button
            icon={<ReloadOutlined spin={isFetching} />}
            onClick={handleRefresh}
            loading={isFetching}
          />
          <Button
            icon={theme === 'dark' ? <SunOutlined /> : <MoonOutlined />}
            onClick={toggleTheme}
          />
          <Button icon={<SettingOutlined />} onClick={() => setConfigModalOpen(true)} />
        </Space>
      </div>

      {/* File List */}
      {config && (
        <div className="file-list">
          {isLoading ? (
            <div className="file-list-loading">
              <Spin />
            </div>
          ) : filteredItems.length === 0 ? (
            <Empty
              image={Empty.PRESENTED_IMAGE_SIMPLE}
              description={searchQuery ? 'No matching files' : 'This folder is empty'}
            />
          ) : viewMode === 'list' ? (
            <>
              {/* Header */}
              <div className="file-list-header">
                <span className="col-name">Name</span>
                <span className="col-size sortable" onClick={toggleSizeSort}>
                  Size
                  {sizeSort === 'asc' && <CaretUpOutlined />}
                  {sizeSort === 'desc' && <CaretDownOutlined />}
                </span>
                <span className="col-date">Modified</span>
                <span className="col-actions">Actions</span>
              </div>

              {/* Items */}
              {filteredItems.map((item) => (
                <div
                  key={item.key}
                  className={`file-item ${item.isFolder ? 'folder' : 'file'}`}
                  onClick={() => handleItemClick(item)}
                >
                  <span className="col-name">
                    {item.isFolder ? (
                      <FolderOutlined className="icon folder-icon" />
                    ) : (
                      <FileOutlined className="icon file-icon" />
                    )}
                    <span className="name">{item.name}</span>
                  </span>
                  <span className="col-size">
                    {item.isFolder
                      ? folderSizes[item.key] === 'loading'
                        ? '...'
                        : typeof folderSizes[item.key] === 'number'
                          ? formatBytes(folderSizes[item.key] as number)
                          : '--'
                      : formatBytes(item.size || 0)}
                  </span>
                  <span className="col-date">
                    {item.lastModified ? formatDate(item.lastModified) : '--'}
                  </span>
                  <span className="col-actions" onClick={(e) => e.stopPropagation()}>
                    {!item.isFolder && (
                      <Popconfirm
                        title="Delete file"
                        description={`Are you sure you want to delete "${item.name}"?`}
                        onConfirm={() => handleDelete(item)}
                        okText="Delete"
                        cancelText="Cancel"
                        okButtonProps={{ danger: true }}
                      >
                        <Button type="text" size="small" danger icon={<DeleteOutlined />} />
                      </Popconfirm>
                    )}
                  </span>
                </div>
              ))}
            </>
          ) : (
            <FileGridView
              items={filteredItems}
              onItemClick={handleItemClick}
              onDelete={handleDelete}
              publicDomain={config?.publicDomain}
              folderSizes={folderSizes}
            />
          )}
        </div>
      )}

      {/* Status Bar */}
      <div className="status-bar">
        <Space size="middle">
          <Tooltip title="Check for updates">
            <Badge dot={!!updateAvailable} offset={[-2, 2]}>
              <Button
                type="text"
                size="small"
                icon={<CloudSyncOutlined spin={checkingUpdate} />}
                onClick={checkForUpdate}
                loading={checkingUpdate}
              >
                v{appVersion || '...'}
              </Button>
            </Badge>
          </Tooltip>
          {config && (
            <span>
              {searchQuery
                ? `${filteredItems.length} of ${items.length} items`
                : `${items.length} items`}
            </span>
          )}
        </Space>
        {config && (
          <span className="domain">
            {config.publicDomain
              ? config.publicDomain
              : config.accessKeyId
                ? `${config.accountId}.r2.cloudflarestorage.com (signed)`
                : `${config.accountId}.r2.cloudflarestorage.com`}
          </span>
        )}
      </div>

      <ConfigModal
        open={configModalOpen}
        onClose={() => config && setConfigModalOpen(false)}
        onSave={(newConfig) => setConfig(newConfig)}
        initialConfig={config}
      />

      <UploadModal
        open={uploadModalOpen}
        onClose={() => setUploadModalOpen(false)}
        currentPath={currentPath}
        config={config}
        onUploadComplete={() => {
          Promise.all([refresh(), refreshSync()]);
        }}
        onCredentialsUpdate={(accessKeyId, secretAccessKey) => {
          if (config) {
            setConfig({ ...config, accessKeyId, secretAccessKey });
          }
        }}
      />

      <FilePreviewModal
        open={!!previewFile}
        onClose={() => setPreviewFile(null)}
        file={previewFile}
        publicDomain={config?.publicDomain}
        accountId={config?.accountId}
        bucket={config?.bucket}
        accessKeyId={config?.accessKeyId}
        secretAccessKey={config?.secretAccessKey}
        onCredentialsUpdate={(accessKeyId, secretAccessKey) => {
          if (config) {
            setConfig({ ...config, accessKeyId, secretAccessKey });
          }
        }}
      />
    </div>
  );
}
