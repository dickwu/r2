'use client';

import React, { useEffect, useState, useCallback, useMemo, useRef } from 'react';
import { Button, Space, App, Spin, Empty, Segmented, Input } from 'antd';
import {
  SettingOutlined,
  ReloadOutlined,
  UploadOutlined,
  SunOutlined,
  MoonOutlined,
  AppstoreOutlined,
  BarsOutlined,
  SearchOutlined,
  DeleteOutlined,
  FolderOutlined,
  DownloadOutlined,
} from '@ant-design/icons';
import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useThemeStore } from '@/app/stores/themeStore';
import ConfigModal, { ModalMode } from '@/app/components/ConfigModal';
import UploadModal from '@/app/components/UploadModal';
import FilePreviewModal from '@/app/components/FilePreviewModal';
import FileRenameModal from '@/app/components/FileRenameModal';
import FolderRenameModal from '@/app/components/FolderRenameModal';
import FileGridView from '@/app/components/FileGridView';
import FileListView from '@/app/components/FileListView';
import AccountSidebar from '@/app/components/AccountSidebar';
import PathBreadcrumb from '@/app/components/PathBreadcrumb';
import StatusBar from '@/app/components/StatusBar';
import BatchDeleteModal from '@/app/components/BatchDeleteModal';
import BatchMoveModal from '@/app/components/BatchMoveModal';
import SyncOverlay from '@/app/components/SyncOverlay';
import DownloadTaskModal from '@/app/components/DownloadTaskModal';
import { useAccountStore, ProviderAccount, Token } from '@/app/stores/accountStore';
import { useR2Files, FileItem } from '@/app/hooks/useR2Files';
import { useFilesSync } from '@/app/hooks/useFilesSync';
import {
  deleteObject,
  renameObject,
  searchFiles,
  listAllObjectsUnderPrefix,
  StorageConfig,
} from '@/app/lib/r2cache';
import { useFolderSizeStore } from '@/app/stores/folderSizeStore';
import { useCurrentPathStore } from '@/app/stores/currentPathStore';
import { useSyncStore } from '@/app/stores/syncStore';
import { useBatchOperationStore } from '@/app/stores/batchOperationStore';
import { useDownloadStore } from '@/app/stores/downloadStore';

type ViewMode = 'list' | 'grid';
type SortOrder = 'asc' | 'desc' | null;

const nameCollator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

export default function Home() {
  // Use Zustand store for account management
  const currentConfig = useAccountStore((state) => state.currentConfig);
  const loading = useAccountStore((state) => state.loading);
  const initialized = useAccountStore((state) => state.initialized);
  const initialize = useAccountStore((state) => state.initialize);
  const hasAccounts = useAccountStore((state) => state.hasAccounts);
  const toStorageConfig = useAccountStore((state) => state.toStorageConfig);

  // Modal state
  const [configModalOpen, setConfigModalOpen] = useState(false);
  const [configModalMode, setConfigModalMode] = useState<ModalMode>('add-account');
  const [editAccount, setEditAccount] = useState<ProviderAccount | null>(null);
  const [editToken, setEditToken] = useState<Token | null>(null);
  const [parentAccountId, setParentAccountId] = useState<string | undefined>();

  const [uploadModalOpen, setUploadModalOpen] = useState(false);
  const [dropQueue, setDropQueue] = useState<string[][]>([]);
  const [previewFile, setPreviewFile] = useState<FileItem | null>(null);
  const [renameFile, setRenameFile] = useState<FileItem | null>(null);
  const [renameFolder, setRenameFolder] = useState<FileItem | null>(null);
  const currentPath = useCurrentPathStore((state) => state.currentPath);
  const setCurrentPath = useCurrentPathStore((state) => state.setCurrentPath);
  const resetCurrentPath = useCurrentPathStore((state) => state.reset);
  const [viewMode, setViewMode] = useState<ViewMode>('list');
  const [searchQuery, setSearchQuery] = useState('');
  const [nameSort, setNameSort] = useState<SortOrder>(null);
  const [sizeSort, setSizeSort] = useState<SortOrder>(null);
  const [modifiedSort, setModifiedSort] = useState<SortOrder>(null);
  const [searchResults, setSearchResults] = useState<FileItem[]>([]);
  const mainContentRef = useRef<HTMLDivElement | null>(null);

  const handleDropHandled = useCallback(() => {
    setDropQueue((prev) => prev.slice(1));
  }, []);

  // Batch operation store
  const selectedKeys = useBatchOperationStore((state) => state.selectedKeys);
  const deleteModalOpen = useBatchOperationStore((state) => state.deleteModalOpen);
  const keysToDelete = useBatchOperationStore((state) => state.keysToDelete);
  const moveModalOpen = useBatchOperationStore((state) => state.moveModalOpen);
  const keysToMove = useBatchOperationStore((state) => state.keysToMove);
  const toggleSelection = useBatchOperationStore((state) => state.toggleSelection);
  const selectAllKeys = useBatchOperationStore((state) => state.selectAll);
  const clearSelection = useBatchOperationStore((state) => state.clearSelection);
  const openDeleteModal = useBatchOperationStore((state) => state.openDeleteModal);
  const closeDeleteModal = useBatchOperationStore((state) => state.closeDeleteModal);
  const setDeleting = useBatchOperationStore((state) => state.setDeleting);
  const openMoveModal = useBatchOperationStore((state) => state.openMoveModal);
  const closeMoveModal = useBatchOperationStore((state) => state.closeMoveModal);
  const setMoving = useBatchOperationStore((state) => state.setMoving);
  const resetBatchOperation = useBatchOperationStore((state) => state.reset);

  // Download store
  const addDownloadTask = useDownloadStore((state) => state.addTask);
  const updateDownloadTask = useDownloadStore((state) => state.updateTask);
  const loadDownloadsFromDatabase = useDownloadStore((state) => state.loadFromDatabase);

  const [searchTotalCount, setSearchTotalCount] = useState(0);
  const [isSearching, setIsSearching] = useState(false);
  const { message } = App.useApp();
  const theme = useThemeStore((s) => s.theme);
  const toggleTheme = useThemeStore((s) => s.toggleTheme);

  const config = useMemo<StorageConfig | null>(
    () => toStorageConfig(),
    [currentConfig, toStorageConfig]
  );
  const isConfigReady = useMemo(() => {
    if (!config?.accountId || !config?.bucket) return false;
    if (config.provider === 'r2') {
      return !!config.accessKeyId && !!config.secretAccessKey;
    }
    if (config.provider === 'aws') {
      return !!config.accessKeyId && !!config.secretAccessKey && !!config.region;
    }
    return (
      !!config.accessKeyId &&
      !!config.secretAccessKey &&
      !!config.endpointHost &&
      !!config.endpointScheme
    );
  }, [config]);

  // Reset path to root when bucket changes
  useEffect(() => {
    resetCurrentPath();
    setSearchQuery('');
    resetBatchOperation();
  }, [
    currentConfig?.bucket,
    currentConfig?.account_id,
    currentConfig?.provider,
    resetBatchOperation,
    resetCurrentPath,
  ]);

  // Load download tasks from database when bucket changes
  useEffect(() => {
    if (!config?.bucket || !config?.accountId) return;

    const loadDownloads = async () => {
      try {
        const sessions = await invoke<
          Array<{
            id: string;
            object_key: string;
            file_name: string;
            file_size: number;
            downloaded_bytes: number;
            local_path: string;
            bucket: string;
            account_id: string;
            status: string;
            error: string | null;
            created_at: number;
            updated_at: number;
          }>
        >('get_download_tasks', {
          bucket: config.bucket,
          accountId: config.accountId,
        });
        loadDownloadsFromDatabase(sessions);
      } catch (e) {
        console.error('Failed to load download tasks:', e);
      }
    };

    loadDownloads();
  }, [config?.bucket, config?.accountId, loadDownloadsFromDatabase]);

  const { items, isLoading, isFetching, error, refresh } = useR2Files(config, currentPath);
  const { isSyncing, isSynced, lastSyncTime, refresh: refreshSync } = useFilesSync(config);

  // Get sync phase for first-load overlay
  const syncPhase = useSyncStore((state) => state.phase);

  // Zustand store for folder metadata
  const metadata = useFolderSizeStore((state) => state.metadata);
  const loadMetadataList = useFolderSizeStore((state) => state.loadMetadataList);
  const clearSizes = useFolderSizeStore((state) => state.clearSizes);

  // Clear folder sizes when bucket/token changes
  useEffect(() => {
    clearSizes();
  }, [currentConfig?.bucket, currentConfig?.account_id, currentConfig?.provider, clearSizes]);

  // Debounced bucket-wide search
  // lastSyncTime is included so search refreshes after file operations (delete/move/rename)
  useEffect(() => {
    if (!searchQuery.trim() || !isSynced) {
      setSearchResults([]);
      setSearchTotalCount(0);
      setIsSearching(false);
      return;
    }

    setIsSearching(true);
    const timer = setTimeout(async () => {
      try {
        const result = await searchFiles(searchQuery);
        // Convert StoredFile to FileItem
        const fileItems: FileItem[] = result.files.map((file) => {
          const parts = file.key.split('/');
          return {
            key: file.key,
            name: parts[parts.length - 1],
            size: file.size,
            lastModified: file.lastModified,
            isFolder: false,
          };
        });
        setSearchResults(fileItems);
        setSearchTotalCount(result.totalCount);
      } catch (e) {
        console.error('Search error:', e);
        setSearchResults([]);
        setSearchTotalCount(0);
      } finally {
        setIsSearching(false);
      }
    }, 300); // 300ms debounce

    return () => clearTimeout(timer);
  }, [searchQuery, isSynced, lastSyncTime]);

  // Filter and sort items - use search results when searching
  const filteredItems = useMemo(() => {
    // When searching, use bucket-wide search results
    let result = searchQuery.trim() ? searchResults : items;

    // Sort by name
    if (nameSort) {
      const useFullPath = searchQuery.trim().length > 0;
      result = [...result].sort((a, b) => {
        if (a.isFolder && !b.isFolder) return -1;
        if (!a.isFolder && b.isFolder) return 1;

        const labelA = useFullPath ? a.key : a.name;
        const labelB = useFullPath ? b.key : b.name;
        const compare = nameCollator.compare(labelA, labelB);
        return nameSort === 'asc' ? compare : -compare;
      });
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
        const dateA = a.isFolder ? metadata[a.key]?.lastModified || '' : a.lastModified || '';
        const dateB = b.isFolder ? metadata[b.key]?.lastModified || '' : b.lastModified || '';

        const timeA = dateA ? new Date(dateA).getTime() : 0;
        const timeB = dateB ? new Date(dateB).getTime() : 0;

        return modifiedSort === 'asc' ? timeA - timeB : timeB - timeA;
      });
    }

    return result;
  }, [items, searchQuery, searchResults, nameSort, sizeSort, modifiedSort, metadata]);

  // Compute total selected file count (folders count as their file count)
  const selectedFileCount = useMemo(() => {
    let count = 0;
    for (const key of selectedKeys) {
      if (key.endsWith('/')) {
        // Folder - use totalFileCount or fileCount from metadata
        const folderMeta = metadata[key];
        const folderCount = folderMeta?.totalFileCount ?? folderMeta?.fileCount;
        count += folderCount ?? 1; // Fallback to 1 if metadata not loaded
      } else {
        // File
        count += 1;
      }
    }
    return count;
  }, [selectedKeys, metadata]);

  // Load folder metadata from directory tree when items change and sync is complete
  // lastSyncTime ensures metadata reloads after refresh (since isSynced stays true during refetch)
  useEffect(() => {
    if (!isSynced || items.length === 0) return;

    const folderKeys = items.filter((item) => item.isFolder).map((item) => item.key);
    if (folderKeys.length > 0) {
      loadMetadataList(folderKeys);
    }
  }, [isSynced, lastSyncTime, items, loadMetadataList]);

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

  useEffect(() => {
    let unlistenDragDrop: UnlistenFn | undefined;

    const setupDragDropListener = async () => {
      try {
        unlistenDragDrop = await getCurrentWindow().onDragDropEvent((event) => {
          if (event.payload.type !== 'drop') return;
          const { paths, position } = event.payload;
          if (!paths || paths.length === 0) return;

          const target = mainContentRef.current;
          if (!target) return;

          const rect = target.getBoundingClientRect();
          const scale = window.devicePixelRatio || 1;
          const x = position.x / scale;
          const y = position.y / scale;

          const isInside = x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
          if (!isInside) return;

          setDropQueue((prev) => [...prev, paths]);
          setUploadModalOpen(true);
        });
      } catch (error) {
        console.error('Failed to listen for drag-drop events:', error);
      }
    };

    setupDragDropListener();

    return () => {
      unlistenDragDrop?.();
    };
  }, []);

  // Start download queue via Rust backend
  const startDownloadQueue = useCallback(async () => {
    if (!config || !isConfigReady) {
      return;
    }

    try {
      await invoke('start_download_queue', {
        config: {
          provider: config.provider,
          account_id: config.accountId,
          bucket: config.bucket,
          access_key_id: config.accessKeyId,
          secret_access_key: config.secretAccessKey,
          region: config.provider === 'aws' ? config.region : null,
          endpoint_scheme: config.provider !== 'r2' ? config.endpointScheme : null,
          endpoint_host: config.provider !== 'r2' ? config.endpointHost : null,
          force_path_style: config.provider === 'r2' ? null : config.forcePathStyle,
        },
      });
    } catch (e) {
      console.error('Failed to start download queue:', e);
    }
  }, [config, isConfigReady]);

  // Keep refs in sync so event listeners always call the latest versions
  const startDownloadQueueRef = useRef(startDownloadQueue);
  startDownloadQueueRef.current = startDownloadQueue;

  const updateDownloadTaskRef = useRef(updateDownloadTask);
  updateDownloadTaskRef.current = updateDownloadTask;

  // Listen for download progress events - ALWAYS active (even when modal is closed)
  // This ensures store is updated regardless of UI state
  useEffect(() => {
    let unlistenProgress: UnlistenFn | undefined;
    let unlistenComplete: UnlistenFn | undefined;

    // Throttle progress updates to prevent React update overflow
    // Store latest progress per task and flush periodically
    const pendingUpdates = new Map<
      string,
      {
        progress: number;
        downloadedBytes: number;
        speed: number;
        status: 'downloading' | 'success';
      }
    >();
    let flushTimeout: ReturnType<typeof setTimeout> | null = null;

    const flushUpdates = () => {
      pendingUpdates.forEach((update, taskId) => {
        updateDownloadTaskRef.current(taskId, update);
      });
      pendingUpdates.clear();
      flushTimeout = null;
    };

    const setupListeners = async () => {
      // Listen for progress updates (throttled)
      unlistenProgress = await listen<{
        task_id: string;
        percent: number;
        downloaded_bytes: number;
        total_bytes: number;
        speed: number;
      }>('download-progress', (event) => {
        const { task_id, percent, downloaded_bytes, speed } = event.payload;

        // Store latest update for this task
        pendingUpdates.set(task_id, {
          progress: percent,
          downloadedBytes: downloaded_bytes,
          speed,
          status: percent >= 100 ? 'success' : 'downloading',
        });

        // Schedule flush if not already scheduled (every 100ms)
        if (!flushTimeout) {
          flushTimeout = setTimeout(flushUpdates, 100);
        }

        // Immediately flush completed downloads
        if (percent >= 100) {
          if (flushTimeout) {
            clearTimeout(flushTimeout);
            flushTimeout = null;
          }
          flushUpdates();
        }
      });

      // Listen for download completion to start next in queue
      unlistenComplete = await listen<string>('download-complete', () => {
        setTimeout(() => {
          startDownloadQueueRef.current();
        }, 100);
      });
    };

    setupListeners();

    return () => {
      unlistenProgress?.();
      unlistenComplete?.();
      if (flushTimeout) {
        clearTimeout(flushTimeout);
      }
    };
  }, []); // Empty deps - only run once on mount

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

  function handleEditAccount(account: ProviderAccount) {
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
    const accountData = accounts.find(
      (a) => a.account.id === currentConfig.account_id && a.provider === currentConfig.provider
    );
    if (!accountData) {
      openAddAccountModal();
      return;
    }

    if (currentConfig.provider === 'r2' && accountData.provider === 'r2') {
      const tokenData = accountData.tokens.find((t) => t.token.id === currentConfig.token_id);
      if (tokenData) {
        handleEditToken(tokenData.token);
        return;
      }
    }

    handleEditAccount(accountData);
  }

  const handleItemClick = useCallback(
    (item: FileItem) => {
      if (item.isFolder) {
        setCurrentPath(item.key);
        setSearchQuery('');
        clearSelection();
      } else {
        setPreviewFile(item);
      }
    },
    [clearSelection]
  );

  const selectAll = useCallback(() => {
    const allKeys = filteredItems.map((item) => item.key);
    selectAllKeys(allKeys);
  }, [filteredItems, selectAllKeys]);

  // Expand folder keys to all contained file keys
  const expandFolderKeys = useCallback(
    async (keys: Set<string>): Promise<Set<string>> => {
      if (!config) return keys;

      const fileKeys = new Set<string>();
      const folderKeys: string[] = [];

      // Separate files and folders
      for (const key of keys) {
        if (key.endsWith('/')) {
          folderKeys.push(key);
        } else {
          fileKeys.add(key);
        }
      }

      // If no folders, return original keys
      if (folderKeys.length === 0) {
        return keys;
      }

      // Expand each folder to its contained files
      for (const folderKey of folderKeys) {
        const objects = await listAllObjectsUnderPrefix(config, folderKey);
        for (const obj of objects) {
          fileKeys.add(obj.key);
        }
      }

      return fileKeys;
    },
    [config]
  );

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
        await deleteObject(config, item.key);
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

  const handleFolderDelete = useCallback(
    async (item: FileItem) => {
      if (!config) return;
      try {
        message.loading({ content: 'Loading folder contents...', key: 'folder-delete' });
        const objects = await listAllObjectsUnderPrefix(config, item.key);
        message.destroy('folder-delete');

        if (objects.length === 0) {
          message.info('Folder is empty');
          return;
        }

        // Set file keys for batch delete modal
        const keys = new Set(objects.map((obj) => obj.key));
        openDeleteModal(keys);
      } catch (e) {
        message.destroy('folder-delete');
        console.error('Folder delete error:', e);
        message.error(
          `Failed to list folder contents: ${e instanceof Error ? e.message : 'Unknown error'}`
        );
      }
    },
    [config, message, openDeleteModal]
  );

  // Download all files in a folder
  const handleFolderDownload = useCallback(
    async (item: FileItem) => {
      if (!config || !item.isFolder || !isConfigReady) return;

      try {
        message.loading({ content: 'Loading folder contents...', key: 'folder-download' });
        const objects = await listAllObjectsUnderPrefix(config, item.key);
        message.destroy('folder-download');

        if (objects.length === 0) {
          message.info('Folder is empty');
          return;
        }

        // Open folder picker dialog
        const folder = await invoke<string | null>('select_download_folder');
        if (!folder) return; // User cancelled

        // Queue all files for download
        for (const obj of objects) {
          const fileName = obj.key.split('/').pop() || obj.key;
          const fileSize = obj.size || 0;
          const taskId = `download-${Date.now()}-${obj.key}`;

          try {
            await invoke('create_download_task', {
              taskId,
              objectKey: obj.key,
              fileName,
              fileSize,
              localPath: folder,
              bucket: config.bucket,
              accountId: config.accountId,
            });
          } catch (e) {
            console.error('Failed to create download task:', e);
            continue;
          }

          addDownloadTask({
            id: taskId,
            key: obj.key,
            fileName,
            fileSize,
            localPath: folder,
          });
        }

        // Start download queue via Rust backend
        setTimeout(() => startDownloadQueue(), 50);

        message.success(`Queued ${objects.length} file(s) for download`);
      } catch (e) {
        message.destroy('folder-download');
        console.error('Folder download error:', e);
        message.error(
          `Failed to download folder: ${e instanceof Error ? e.message : 'Unknown error'}`
        );
      }
    },
    [config, isConfigReady, message, addDownloadTask, startDownloadQueue]
  );

  const openBatchDeleteConfirm = useCallback(async () => {
    if (selectedKeys.size === 0) return;

    // Snapshot the current selection
    const currentSelection = new Set(selectedKeys);

    // Check if any folders are selected
    const hasFolders = Array.from(currentSelection).some((key) => key.endsWith('/'));

    let finalKeys = currentSelection;
    if (hasFolders) {
      message.loading({ content: 'Preparing files...', key: 'batch-prep' });
      finalKeys = await expandFolderKeys(currentSelection);
      message.destroy('batch-prep');

      if (finalKeys.size === 0) {
        message.info('No files to delete');
        return;
      }
    }

    openDeleteModal(finalKeys);
  }, [selectedKeys, expandFolderKeys, message, openDeleteModal]);

  const handleBatchDeleteSuccess = useCallback(async () => {
    clearSelection();
    await Promise.all([refresh(), refreshSync()]);
  }, [refresh, refreshSync, clearSelection]);

  const openBatchMoveModalHandler = useCallback(async () => {
    if (selectedKeys.size === 0) return;

    // Snapshot the current selection
    const currentSelection = new Set(selectedKeys);

    // Check if any folders are selected
    const hasFolders = Array.from(currentSelection).some((key) => key.endsWith('/'));

    let finalKeys = currentSelection;
    if (hasFolders) {
      message.loading({ content: 'Preparing files...', key: 'batch-prep' });
      finalKeys = await expandFolderKeys(currentSelection);
      message.destroy('batch-prep');

      if (finalKeys.size === 0) {
        message.info('No files to move');
        return;
      }
    }

    openMoveModal(finalKeys);
  }, [selectedKeys, expandFolderKeys, message, openMoveModal]);

  const handleBatchMoveSuccess = useCallback(async () => {
    clearSelection();
    await Promise.all([refresh(), refreshSync()]);
  }, [refresh, refreshSync, clearSelection]);

  const handleRenameClick = useCallback((item: FileItem) => {
    setRenameFile(item);
  }, []);

  const handleFolderRenameClick = useCallback((item: FileItem) => {
    if (item.isFolder) {
      setRenameFolder(item);
    }
  }, []);

  // Download single file
  const handleDownload = useCallback(
    async (item: FileItem) => {
      if (!config || item.isFolder || !isConfigReady) return;

      try {
        // Open folder picker dialog
        const folder = await invoke<string | null>('select_download_folder');
        if (!folder) return; // User cancelled

        const taskId = `download-${Date.now()}-${item.key}`;
        const fileSize = item.size || 0;

        // Create task in database first
        await invoke('create_download_task', {
          taskId,
          objectKey: item.key,
          fileName: item.name,
          fileSize,
          localPath: folder,
          bucket: config.bucket,
          accountId: config.accountId,
        });

        // Add task to download store (as pending)
        addDownloadTask({
          id: taskId,
          key: item.key,
          fileName: item.name,
          fileSize,
          localPath: folder,
        });

        // Start download queue via Rust backend
        setTimeout(() => startDownloadQueue(), 50);
      } catch (e) {
        console.error('Download error:', e);
        message.error(`Failed to download: ${e instanceof Error ? e.message : 'Unknown error'}`);
      }
    },
    [config, isConfigReady, message, addDownloadTask, startDownloadQueue]
  );

  // Batch download selected files
  const handleBatchDownload = useCallback(async () => {
    if (selectedKeys.size === 0 || !config || !isConfigReady) return;

    try {
      // Expand folders to get all files
      const currentSelection = new Set(selectedKeys);
      const hasFolders = Array.from(currentSelection).some((key) => key.endsWith('/'));

      let fileKeys = currentSelection;
      if (hasFolders) {
        message.loading({ content: 'Preparing files...', key: 'batch-download-prep' });
        fileKeys = await expandFolderKeys(currentSelection);
        message.destroy('batch-download-prep');

        if (fileKeys.size === 0) {
          message.info('No files to download');
          return;
        }
      }

      // Filter out folders from the keys
      const filesToDownload = Array.from(fileKeys).filter((key) => !key.endsWith('/'));

      if (filesToDownload.length === 0) {
        message.info('No files to download');
        return;
      }

      // Open folder picker dialog
      const folder = await invoke<string | null>('select_download_folder');
      if (!folder) return; // User cancelled

      // Add tasks to download store (as pending)
      // Rust backend will look up file sizes from cache when fileSize is 0
      for (const key of filesToDownload) {
        const fileName = key.split('/').pop() || key;
        const fileSize = filteredItems.find((item) => item.key === key)?.size ?? 0;
        const taskId = `download-${Date.now()}-${key}`;

        // Create task in database first (Rust looks up file size from cache if 0)
        try {
          await invoke('create_download_task', {
            taskId,
            objectKey: key,
            fileName,
            fileSize,
            localPath: folder,
            bucket: config.bucket,
            accountId: config.accountId,
          });
        } catch (e) {
          console.error('Failed to create download task:', e);
          continue;
        }

        addDownloadTask({
          id: taskId,
          key,
          fileName,
          fileSize,
          localPath: folder,
        });
      }

      // Start download queue via Rust backend
      setTimeout(() => startDownloadQueue(), 50);

      message.success(`Queued ${filesToDownload.length} file(s) for download`);
    } catch (e) {
      console.error('Batch download error:', e);
      message.error(
        `Failed to start downloads: ${e instanceof Error ? e.message : 'Unknown error'}`
      );
    }
  }, [
    selectedKeys,
    config,
    isConfigReady,
    message,
    expandFolderKeys,
    filteredItems,
    addDownloadTask,
    startDownloadQueue,
  ]);

  const handleRename = useCallback(
    async (newPath: string) => {
      if (!renameFile || !config || !isConfigReady) return;

      await renameObject(config, renameFile.key, newPath);

      // Close preview modal if the renamed file is currently being previewed
      if (previewFile?.key === renameFile.key) {
        setPreviewFile(null);
      }

      // Refresh file list
      await Promise.all([refresh(), refreshSync()]);
    },
    [renameFile, config, isConfigReady, previewFile, refresh, refreshSync]
  );

  const handleFolderRenameSuccess = useCallback(async () => {
    await Promise.all([refresh(), refreshSync()]);
  }, [refresh, refreshSync]);

  function toggleSizeSort() {
    setSizeSort((prev) => {
      if (prev === null) return 'desc';
      if (prev === 'desc') return 'asc';
      return null;
    });
    // Clear modified sort when size sort is activated
    setModifiedSort(null);
    setNameSort(null);
  }

  function toggleModifiedSort() {
    setModifiedSort((prev) => {
      if (prev === null) return 'desc';
      if (prev === 'desc') return 'asc';
      return null;
    });
    // Clear size sort when modified sort is activated
    setSizeSort(null);
    setNameSort(null);
  }

  function toggleNameSort() {
    setNameSort((prev) => {
      if (prev === null) return 'desc';
      if (prev === 'desc') return 'asc';
      return null;
    });
    setSizeSort(null);
    setModifiedSort(null);
  }

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
      <div className="main-content" ref={mainContentRef}>
        <div className="file-manager">
          {/* Toolbar */}
          <div className="toolbar">
            <Space>
              <PathBreadcrumb
                bucketName={currentConfig?.bucket || 'Root'}
                onNavigate={() => setSearchQuery('')}
              />
              {selectedKeys.size > 0 && (
                <>
                  <span style={{ color: 'var(--text-secondary)' }}>
                    {selectedFileCount !== selectedKeys.size
                      ? `${selectedKeys.size} items (${selectedFileCount.toLocaleString()} files)`
                      : `${selectedKeys.size} selected`}
                  </span>
                  <Button size="small" icon={<DownloadOutlined />} onClick={handleBatchDownload}>
                    Download
                  </Button>
                  <Button
                    size="small"
                    icon={<FolderOutlined />}
                    onClick={openBatchMoveModalHandler}
                  >
                    Move
                  </Button>
                  <Button
                    size="small"
                    danger
                    icon={<DeleteOutlined />}
                    onClick={openBatchDeleteConfirm}
                  >
                    Delete
                  </Button>
                  <Button size="small" onClick={clearSelection}>
                    Clear
                  </Button>
                </>
              )}
            </Space>
            <Space>
              <Input
                placeholder="Search all files in bucket..."
                prefix={<SearchOutlined />}
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                allowClear
                autoComplete="off"
                autoCorrect="off"
                autoCapitalize="off"
                style={{ width: 220 }}
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
          <div className="file-list">
            {!config ? (
              <Empty
                image={Empty.PRESENTED_IMAGE_SIMPLE}
                description="Select a bucket from the sidebar to get started"
              />
            ) : lastSyncTime === null && (isSyncing || syncPhase !== 'idle') ? (
              /* Show sync overlay during initial sync (before cache is ready) */
              <SyncOverlay />
            ) : isLoading || isSearching ? (
              <div className="file-list-loading">
                <Spin tip={isSearching ? 'Searching bucket...' : undefined} fullscreen />
              </div>
            ) : filteredItems.length === 0 ? (
              <Empty
                image={Empty.PRESENTED_IMAGE_SIMPLE}
                description={
                  searchQuery
                    ? `No files matching "${searchQuery}" in bucket`
                    : 'This folder is empty'
                }
              />
            ) : viewMode === 'list' ? (
              <FileListView
                items={filteredItems}
                selectedKeys={selectedKeys}
                metadata={metadata}
                nameSort={nameSort}
                sizeSort={sizeSort}
                modifiedSort={modifiedSort}
                showFullPath={!!searchQuery.trim()}
                onItemClick={handleItemClick}
                onToggleSelection={toggleSelection}
                onSelectAll={selectAll}
                onClearSelection={clearSelection}
                onToggleNameSort={toggleNameSort}
                onToggleSizeSort={toggleSizeSort}
                onToggleModifiedSort={toggleModifiedSort}
                onDelete={handleDelete}
                onRename={handleRenameClick}
                onDownload={handleDownload}
                onFolderDelete={handleFolderDelete}
                onFolderDownload={handleFolderDownload}
                onFolderRename={handleFolderRenameClick}
              />
            ) : (
              <FileGridView
                items={filteredItems}
                onItemClick={handleItemClick}
                onDelete={handleDelete}
                onRename={handleRenameClick}
                onDownload={handleDownload}
                onFolderDelete={handleFolderDelete}
                onFolderDownload={handleFolderDownload}
                onFolderRename={handleFolderRenameClick}
                storageConfig={config}
                folderSizes={metadata}
                selectedKeys={selectedKeys}
                onToggleSelection={toggleSelection}
                showFullPath={!!searchQuery.trim()}
              />
            )}
          </div>

          {/* Status Bar */}
          <StatusBar
            totalItemsCount={items.length}
            searchQuery={searchQuery}
            searchTotalCount={searchTotalCount}
            hasConfig={!!config}
            isLoadingFiles={isLoading || isSyncing}
            storageConfig={config}
          />

          <ConfigModal
            open={configModalOpen}
            onClose={(force) => {
              if (force || currentConfig || hasAccounts()) {
                setConfigModalOpen(false);
              }
            }}
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
            dropQueue={dropQueue}
            onDropHandled={handleDropHandled}
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
            config={config}
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
          />

          <FolderRenameModal
            open={!!renameFolder}
            onClose={() => setRenameFolder(null)}
            folder={renameFolder}
            folderMetadata={renameFolder ? metadata[renameFolder.key] : undefined}
            config={config}
            onSuccess={handleFolderRenameSuccess}
          />

          <BatchDeleteModal
            open={deleteModalOpen}
            selectedKeys={keysToDelete}
            config={config}
            onClose={closeDeleteModal}
            onSuccess={handleBatchDeleteSuccess}
            onDeletingChange={setDeleting}
          />

          <BatchMoveModal
            open={moveModalOpen}
            selectedKeys={keysToMove}
            config={config}
            onClose={closeMoveModal}
            onSuccess={handleBatchMoveSuccess}
            onMovingChange={setMoving}
          />

          <DownloadTaskModal storageConfig={config} />
        </div>
      </div>
    </div>
  );
}
