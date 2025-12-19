'use client';

import { useEffect, useState, useCallback, useMemo } from 'react';
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
} from '@ant-design/icons';
import { useTheme } from './providers';
import ConfigModal, { ModalMode } from './components/ConfigModal';
import UploadModal from './components/UploadModal';
import FilePreviewModal from './components/FilePreviewModal';
import FileGridView from './components/FileGridView';
import AccountSidebar from './components/AccountSidebar';
import { useAccountStore, Account, Token } from './stores/accountStore';
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
  // Use Zustand store for account management
  const currentConfig = useAccountStore((state) => state.currentConfig);
  const loading = useAccountStore((state) => state.loading);
  const initialized = useAccountStore((state) => state.initialized);
  const initialize = useAccountStore((state) => state.initialize);
  const hasAccounts = useAccountStore((state) => state.hasAccounts);
  const toR2Config = useAccountStore((state) => state.toR2Config);

  // Modal state
  const [configModalOpen, setConfigModalOpen] = useState(false);
  const [configModalMode, setConfigModalMode] = useState<ModalMode>('add-account');
  const [editAccount, setEditAccount] = useState<Account | null>(null);
  const [editToken, setEditToken] = useState<Token | null>(null);
  const [parentAccountId, setParentAccountId] = useState<string | undefined>();

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

  // Convert to R2Config for hooks compatibility
  const config = useMemo(() => toR2Config(), [currentConfig]);

  // Reset path to root when bucket changes
  useEffect(() => {
    setCurrentPath('');
    setSearchQuery('');
  }, [currentConfig?.bucket, currentConfig?.token_id]);

  const { items, isLoading, isFetching, error, refresh } = useR2Files(config, currentPath);
  const { isSynced, refresh: refreshSync } = useFilesSync(config);

  // Zustand store for folder sizes
  const folderSizes = useFolderSizeStore((state) => state.sizes);
  const calculateSizes = useFolderSizeStore((state) => state.calculateSizes);
  const clearSizes = useFolderSizeStore((state) => state.clearSizes);

  // Clear folder sizes when bucket/token changes
  useEffect(() => {
    clearSizes();
  }, [currentConfig?.bucket, currentConfig?.token_id, clearSizes]);

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

  // Initialize store on mount
  useEffect(() => {
    initialize();
    loadVersion();
  }, []);

  // Open add account modal if no accounts exist after initialization
  useEffect(() => {
    if (initialized && !loading && !hasAccounts()) {
      openAddAccountModal();
    }
  }, [initialized, loading]);

  // Open add account modal if no current config after initialization
  useEffect(() => {
    if (initialized && !loading && hasAccounts() && !currentConfig) {
      openAddAccountModal();
    }
  }, [initialized, loading, currentConfig]);

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

  function openAddAccountModal() {
    setConfigModalMode('add-account');
    setEditAccount(null);
    setEditToken(null);
    setParentAccountId(undefined);
    setConfigModalOpen(true);
  }

  function handleAddAccount() {
    openAddAccountModal();
  }

  function handleEditAccount(account: Account) {
    setConfigModalMode('edit-account');
    setEditAccount(account);
    setEditToken(null);
    setParentAccountId(undefined);
    setConfigModalOpen(true);
  }

  function handleAddToken(accountId: string) {
    setConfigModalMode('add-token');
    setEditAccount(null);
    setEditToken(null);
    setParentAccountId(accountId);
    setConfigModalOpen(true);
  }

  function handleEditToken(token: Token) {
    setConfigModalMode('edit-token');
    setEditAccount(null);
    setEditToken(token);
    setParentAccountId(undefined);
    setConfigModalOpen(true);
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
          <HomeOutlined /> {currentConfig?.bucket || 'Root'}
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
    <div className="app-layout">
      {/* Sidebar */}
      <AccountSidebar
        onAddAccount={handleAddAccount}
        onEditAccount={handleEditAccount}
        onAddToken={handleAddToken}
        onEditToken={handleEditToken}
      />

      {/* Main Content */}
      <div className="main-content">
        <div className="file-manager">
          {/* Toolbar */}
          <div className="toolbar">
            <Space>
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
              <Button
                type="primary"
                icon={<UploadOutlined />}
                onClick={() => setUploadModalOpen(true)}
              >
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
              <Button icon={<SettingOutlined />} onClick={openAddAccountModal} />
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
            {currentConfig && (
              <span className="domain">
                {currentConfig.public_domain
                  ? currentConfig.public_domain
                  : currentConfig.access_key_id
                    ? `${currentConfig.account_id}.r2.cloudflarestorage.com (signed)`
                    : `${currentConfig.account_id}.r2.cloudflarestorage.com`}
              </span>
            )}
          </div>

          <ConfigModal
            open={configModalOpen}
            onClose={() => currentConfig && setConfigModalOpen(false)}
            mode={configModalMode}
            editAccount={editAccount}
            editToken={editToken}
            parentAccountId={parentAccountId}
          />

          <UploadModal
            open={uploadModalOpen}
            onClose={() => setUploadModalOpen(false)}
            currentPath={currentPath}
            config={config}
            onUploadComplete={() => {
              Promise.all([refresh(), refreshSync()]);
            }}
            onCredentialsUpdate={() => {
              // Credentials are now managed through the store
              initialize();
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
            onCredentialsUpdate={() => {
              // Credentials are now managed through the store
              initialize();
            }}
          />
        </div>
      </div>
    </div>
  );
}
