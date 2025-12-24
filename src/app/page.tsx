'use client';

import { useEffect, useState, useCallback, useMemo } from 'react';
import {
  Button,
  Breadcrumb,
  Space,
  App,
  Spin,
  Empty,
  Segmented,
  Input,
  Popconfirm,
  Checkbox,
  Modal,
} from 'antd';
import {
  SettingOutlined,
  ReloadOutlined,
  UploadOutlined,
  HomeOutlined,
  SunOutlined,
  MoonOutlined,
  AppstoreOutlined,
  BarsOutlined,
  SearchOutlined,
  DeleteOutlined,
} from '@ant-design/icons';
import { useTheme } from './providers';
import ConfigModal, { ModalMode } from './components/ConfigModal';
import UploadModal from './components/UploadModal';
import FilePreviewModal from './components/FilePreviewModal';
import FileRenameModal from './components/FileRenameModal';
import FileGridView from './components/FileGridView';
import FileListView from './components/FileListView';
import AccountSidebar from './components/AccountSidebar';
import StatusBar from './components/StatusBar';
import { useAccountStore, Account, Token } from './stores/accountStore';
import { useR2Files, FileItem } from './hooks/useR2Files';
import { useFilesSync } from './hooks/useFilesSync';
import { deleteR2Object, renameR2Object } from './lib/r2cache';
import { useFolderSizeStore } from './stores/folderSizeStore';
import { formatBytes } from './utils/formatBytes';

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
  const [renameFile, setRenameFile] = useState<FileItem | null>(null);
  const [currentPath, setCurrentPath] = useState('');
  const [viewMode, setViewMode] = useState<ViewMode>('list');
  const [searchQuery, setSearchQuery] = useState('');
  const [sizeSort, setSizeSort] = useState<SortOrder>(null);
  const [modifiedSort, setModifiedSort] = useState<SortOrder>(null);
  const [selectedKeys, setSelectedKeys] = useState<Set<string>>(new Set());
  const [deleteConfirmOpen, setDeleteConfirmOpen] = useState(false);
  const [deleteConfirmInput, setDeleteConfirmInput] = useState('');
  const [isDeleting, setIsDeleting] = useState(false);
  const { message } = App.useApp();
  const { theme, toggleTheme } = useTheme();

  // Convert to R2Config for hooks compatibility
  const config = useMemo(() => toR2Config(), [currentConfig]);

  // Reset path to root when bucket changes
  useEffect(() => {
    setCurrentPath('');
    setSearchQuery('');
    setSelectedKeys(new Set());
  }, [currentConfig?.bucket, currentConfig?.token_id]);

  const { items, isLoading, isFetching, error, refresh } = useR2Files(config, currentPath);
  const { isSyncing, isSynced, refresh: refreshSync } = useFilesSync(config);

  // Zustand store for folder metadata
  const metadata = useFolderSizeStore((state) => state.metadata);
  const loadMetadataList = useFolderSizeStore((state) => state.loadMetadataList);
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
          ? typeof metadata[a.key]?.size === 'number'
            ? (metadata[a.key].size as number)
            : 0
          : a.size || 0;
        const sizeB = b.isFolder
          ? typeof metadata[b.key]?.size === 'number'
            ? (metadata[b.key].size as number)
            : 0
          : b.size || 0;
        return sizeSort === 'asc' ? sizeA - sizeB : sizeB - sizeA;
      });
    }

    // Sort by modified date
    if (modifiedSort) {
      result = [...result].sort((a, b) => {
        const dateA = a.isFolder
          ? metadata[a.key]?.lastModified || ''
          : a.lastModified || '';
        const dateB = b.isFolder
          ? metadata[b.key]?.lastModified || ''
          : b.lastModified || '';
        
        const timeA = dateA ? new Date(dateA).getTime() : 0;
        const timeB = dateB ? new Date(dateB).getTime() : 0;
        
        return modifiedSort === 'asc' ? timeA - timeB : timeB - timeA;
      });
    }

    return result;
  }, [items, searchQuery, sizeSort, modifiedSort, metadata]);

  // Load folder metadata from directory tree when items change and sync is complete
  useEffect(() => {
    if (!isSynced || items.length === 0) return;

    const folderKeys = items.filter((item) => item.isFolder).map((item) => item.key);
    if (folderKeys.length > 0) {
      loadMetadataList(folderKeys);
    }
  }, [isSynced, items, loadMetadataList]);

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

  function handleSettingsClick() {
    const accounts = useAccountStore.getState().accounts;
    if (!currentConfig) {
      openAddAccountModal();
      return;
    }
    // Find current token from accounts
    const accountData = accounts.find((a) => a.account.id === currentConfig.account_id);
    const tokenData = accountData?.tokens.find((t) => t.token.id === currentConfig.token_id);
    if (tokenData) {
      handleEditToken(tokenData.token);
    } else {
      openAddAccountModal();
    }
  }

  const handleItemClick = useCallback((item: FileItem) => {
    if (item.isFolder) {
      setCurrentPath(item.key);
      setSearchQuery('');
      setSelectedKeys(new Set());
    } else {
      setPreviewFile(item);
    }
  }, []);

  const toggleSelection = useCallback((key: string) => {
    setSelectedKeys((prev) => {
      const next = new Set(prev);
      if (next.has(key)) {
        next.delete(key);
      } else {
        next.add(key);
      }
      return next;
    });
  }, []);

  const selectAll = useCallback(() => {
    const fileKeys = filteredItems.filter((item) => !item.isFolder).map((item) => item.key);
    setSelectedKeys(new Set(fileKeys));
  }, [filteredItems]);

  const clearSelection = useCallback(() => {
    setSelectedKeys(new Set());
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

  const openBatchDeleteConfirm = useCallback(() => {
    setDeleteConfirmInput('');
    setDeleteConfirmOpen(true);
  }, []);

  const closeBatchDeleteConfirm = useCallback(() => {
    setDeleteConfirmOpen(false);
    setDeleteConfirmInput('');
  }, []);

  const handleBatchDelete = useCallback(async () => {
    if (!config || selectedKeys.size === 0) return;

    const keys = Array.from(selectedKeys);
    const count = keys.length;

    // Close confirmation modal
    closeBatchDeleteConfirm();
    
    // Show full-screen loading
    setIsDeleting(true);

    try {
      // Delete all selected files
      await Promise.all(keys.map((key) => deleteR2Object(config, key)));

      message.success(`Deleted ${count} file${count > 1 ? 's' : ''}`);
      setSelectedKeys(new Set());

      // Refresh file list after deletion
      await Promise.all([refresh(), refreshSync()]);
    } catch (e) {
      console.error('Batch delete error:', e);
      message.error(`Failed to delete files: ${e instanceof Error ? e.message : 'Unknown error'}`);
    } finally {
      setIsDeleting(false);
    }
  }, [config, selectedKeys, message, refresh, refreshSync, closeBatchDeleteConfirm]);

  const handleRenameClick = useCallback((item: FileItem) => {
    setRenameFile(item);
  }, []);

  const handleRename = useCallback(
    async (newPath: string) => {
      if (!renameFile || !currentConfig) return;

      // Check if S3 credentials are available
      if (!currentConfig.access_key_id || !currentConfig.secret_access_key) {
        throw new Error('S3 credentials required for rename/move operation');
      }

      // Perform rename using S3 API
      await renameR2Object(
        {
          accountId: currentConfig.account_id,
          bucket: currentConfig.bucket,
          accessKeyId: currentConfig.access_key_id,
          secretAccessKey: currentConfig.secret_access_key,
        },
        renameFile.key,
        newPath
      );

      // Close preview modal if the renamed file is currently being previewed
      if (previewFile?.key === renameFile.key) {
        setPreviewFile(null);
      }

      // Refresh file list
      await Promise.all([refresh(), refreshSync()]);
    },
    [renameFile, currentConfig, previewFile, refresh, refreshSync]
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
    // Clear modified sort when size sort is activated
    setModifiedSort(null);
  }

  function toggleModifiedSort() {
    setModifiedSort((prev) => {
      if (prev === null) return 'desc';
      if (prev === 'desc') return 'asc';
      return null;
    });
    // Clear size sort when modified sort is activated
    setSizeSort(null);
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

  if (loading || isDeleting) {
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
              {selectedKeys.size > 0 && (
                <>
                  <span style={{ color: 'var(--text-secondary)' }}>
                    {selectedKeys.size} selected
                  </span>
                  <Button
                    size="small"
                    danger
                    icon={<DeleteOutlined />}
                    onClick={openBatchDeleteConfirm}
                  >
                    Delete Selected
                  </Button>
                  <Button size="small" onClick={clearSelection}>
                    Clear
                  </Button>
                </>
              )}
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
              <Button icon={<SettingOutlined />} onClick={handleSettingsClick} />
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
                <FileListView
                  items={filteredItems}
                  selectedKeys={selectedKeys}
                  metadata={metadata}
                  sizeSort={sizeSort}
                  modifiedSort={modifiedSort}
                  onItemClick={handleItemClick}
                  onToggleSelection={toggleSelection}
                  onSelectAll={selectAll}
                  onClearSelection={clearSelection}
                  onToggleSizeSort={toggleSizeSort}
                  onToggleModifiedSort={toggleModifiedSort}
                  onDelete={handleDelete}
                  onRename={handleRenameClick}
                />
              ) : (
                <FileGridView
                  items={filteredItems}
                  onItemClick={handleItemClick}
                  onDelete={handleDelete}
                  onRename={handleRenameClick}
                  publicDomain={config?.publicDomain}
                  folderSizes={metadata}
                  selectedKeys={selectedKeys}
                  onToggleSelection={toggleSelection}
                />
              )}
            </div>
          )}

          {/* Status Bar */}
          <StatusBar
            filteredItemsCount={filteredItems.length}
            totalItemsCount={items.length}
            searchQuery={searchQuery}
            hasConfig={!!config}
            currentConfig={currentConfig}
            isSyncing={isSyncing}
          />

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

          <FileRenameModal
            open={!!renameFile}
            onClose={() => setRenameFile(null)}
            fileName={renameFile?.name || ''}
            filePath={renameFile?.key || ''}
            onRename={handleRename}
            config={config}
          />

          <Modal
            title="Confirm Batch Delete"
            open={deleteConfirmOpen}
            onCancel={closeBatchDeleteConfirm}
            onOk={handleBatchDelete}
            okText="Delete"
            okButtonProps={{
              danger: true,
              disabled: deleteConfirmInput !== selectedKeys.size.toString(),
            }}
          >
            <p>
              You are about to delete <strong>{selectedKeys.size}</strong> file
              {selectedKeys.size > 1 ? 's' : ''}.
            </p>
            <p>This action cannot be undone.</p>
            <p style={{ marginTop: 16, marginBottom: 8 }}>
              Please type <strong>{selectedKeys.size}</strong> to confirm:
            </p>
            <Input
              value={deleteConfirmInput}
              onChange={(e) => setDeleteConfirmInput(e.target.value)}
              placeholder={`Type ${selectedKeys.size} to confirm`}
              autoFocus
              onPressEnter={() => {
                if (deleteConfirmInput === selectedKeys.size.toString()) {
                  handleBatchDelete();
                }
              }}
            />
          </Modal>
        </div>
      </div>
    </div>
  );
}
