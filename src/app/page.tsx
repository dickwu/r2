'use client';

import { t as i18nT } from '@/i18n';
import { useTranslation } from 'react-i18next';
import dynamic from 'next/dynamic';
import React, { useEffect, useState, useCallback, useMemo, useRef } from 'react';
import { Alert, App, Button, Spin, type InputRef } from 'antd';
import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useThemeStore } from '@/app/stores/themeStore';
import { usePreviewStore } from '@/app/stores/previewStore';
import type { ModalMode } from '@/app/components/ConfigModal';
import type { SettingsTab } from '@/app/components/SettingsModal';
import FileGridView from '@/app/components/FileGridView';
import FileListView from '@/app/components/FileListView';
import FileColumnView from '@/app/components/FileColumnView';
import AccountSidebar from '@/app/components/AccountSidebar';
import Toolbar from '@/app/components/Toolbar';
import StatusBar from '@/app/components/StatusBar';
import Titlebar from '@/app/components/Titlebar';
import SyncBanner from '@/app/components/SyncBanner';
import SelectionActionBar from '@/app/components/SelectionActionBar';
import Inspector from '@/app/components/Inspector';
import EmptyState from '@/app/components/EmptyState';
import { useAccountStore, ProviderAccount, Token } from '@/app/stores/accountStore';
import { useR2Files, FileItem } from '@/app/hooks/useR2Files';
import { useFilesSync } from '@/app/hooks/useFilesSync';
import {
  deleteObject,
  searchFiles,
  listAllObjectsUnderPrefix,
  StorageConfig,
} from '@/app/lib/r2cache';
import { useFolderSizeStore } from '@/app/stores/folderSizeStore';
import { useCurrentPathStore } from '@/app/stores/currentPathStore';
import { useSyncStore } from '@/app/stores/syncStore';
import { useBatchOperationStore } from '@/app/stores/batchOperationStore';
import {
  setupGlobalDownloadListeners,
  useDownloadStore,
  type DownloadSession,
} from '@/app/stores/downloadStore';
import { setupGlobalRenameListeners, useRenameStore } from '@/app/stores/renameStore';
import { setupGlobalMountListeners, useMountStore } from '@/app/stores/mountStore';
import { useKeyboardShortcuts } from '@/app/hooks/useKeyboardShortcuts';
import { useGlobalShortcuts } from '@/app/hooks/useGlobalShortcuts';
import type { CommandAction } from '@/app/components/CommandPalette';
import { useMoveStore, setupGlobalMoveListeners, loadAllActiveMoves } from '@/app/stores/moveStore';
import { useReportStore } from '@/app/stores/reportStore';
import { useTransferErrorStore } from '@/app/stores/transferErrorStore';
import TransferDock from '@/app/components/TransferDock';
import Toast from '@/app/components/Toast';
import { useToastStore } from '@/app/stores/toastStore';
import { useViewModeStore } from '@/app/stores/viewModeStore';

const ConfigModal = dynamic(() => import('@/app/components/ConfigModal'), { ssr: false });
const SettingsModal = dynamic(() => import('@/app/components/SettingsModal'), { ssr: false });
const UploadModal = dynamic(() => import('@/app/components/UploadModal'), { ssr: false });
const FilePreviewModal = dynamic(() => import('@/app/components/FilePreviewModal'), { ssr: false });
const FileRenameModal = dynamic(() => import('@/app/components/FileRenameModal'), { ssr: false });
const FolderRenameModal = dynamic(() => import('@/app/components/FolderRenameModal'), {
  ssr: false,
});
const BatchDeleteModal = dynamic(() => import('@/app/components/BatchDeleteModal'), { ssr: false });
const BatchMoveModal = dynamic(() => import('@/app/components/BatchMoveModal'), { ssr: false });
const DownloadTaskModal = dynamic(() => import('@/app/components/DownloadTaskModal'), {
  ssr: false,
});
const MoveTaskModal = dynamic(() => import('@/app/components/MoveTaskModal'), { ssr: false });
const MountModal = dynamic(() => import('@/app/components/MountModal'), { ssr: false });
const CommandPalette = dynamic(() => import('@/app/components/CommandPalette'), { ssr: false });
const ReportProblemModal = dynamic(() => import('@/app/components/report/ReportProblemModal'), {
  ssr: false,
});
const TransferErrorModal = dynamic(() => import('@/app/components/TransferErrorModal'), {
  ssr: false,
});

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

  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsTab, setSettingsTab] = useState<SettingsTab>('appearance');

  const [uploadModalOpen, setUploadModalOpen] = useState(false);
  const [dropQueue, setDropQueue] = useState<string[][]>([]);
  const previewFile = usePreviewStore((s) => s.file);
  const openPreview = usePreviewStore((s) => s.open);
  const closePreview = usePreviewStore((s) => s.close);
  const [renameFile, setRenameFile] = useState<FileItem | null>(null);
  const [renameFolder, setRenameFolder] = useState<FileItem | null>(null);
  const currentPath = useCurrentPathStore((state) => state.currentPath);
  const setCurrentPath = useCurrentPathStore((state) => state.setCurrentPath);
  const resetCurrentPath = useCurrentPathStore((state) => state.reset);
  const viewMode = useViewModeStore((s) => s.viewMode);
  const setViewMode = useViewModeStore((s) => s.setViewMode);
  const cycleViewMode = useViewModeStore((s) => s.cycleViewMode);
  const [searchQuery, setSearchQuery] = useState('');
  const [nameSort, setNameSort] = useState<SortOrder>(null);
  const [sizeSort, setSizeSort] = useState<SortOrder>(null);
  const [modifiedSort, setModifiedSort] = useState<SortOrder>(null);
  const [searchResults, setSearchResults] = useState<FileItem[]>([]);
  const mainContentRef = useRef<HTMLDivElement | null>(null);
  const searchInputRef = useRef<InputRef | null>(null);
  const [isDragOver, setIsDragOver] = useState(false);
  const [focusedItem, setFocusedItem] = useState<FileItem | null>(null);
  const showInspector = useThemeStore((s) => s.showInspector);
  const setShowInspector = useThemeStore((s) => s.setShowInspector);

  // Command palette state
  const [paletteOpen, setPaletteOpen] = useState(false);
  const pushToast = useToastStore((s) => s.pushToast);
  const toggleTheme = useThemeStore((s) => s.toggleTheme);

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
  const addDownloadTasks = useDownloadStore((state) => state.addTasks);
  const loadDownloadsFromDatabase = useDownloadStore((state) => state.loadFromDatabase);

  const [searchTotalCount, setSearchTotalCount] = useState(0);
  const [isSearching, setIsSearching] = useState(false);
  const { message } = App.useApp();
  const { t } = useTranslation();
  const sidebarStyle = useThemeStore((s) => s.sidebarStyle);
  const mountModalOpen = useMountStore((s) => s.modalOpen);
  const moveTaskModalOpen = useMoveStore((s) => s.modalOpen);
  const downloadTaskModalOpen = useDownloadStore((s) => s.modalOpen);
  const reportOpen = useReportStore((s) => s.isOpen);
  const transferErrorOpen = useTransferErrorStore((s) => s.isOpen);

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

  const { items, hasData, isLoading, isFetching, isPartial, isCached, error, refresh } = useR2Files(
    config,
    currentPath
  );
  const { isSyncing, isSynced, lastSyncTime, refresh: refreshSync } = useFilesSync(config);

  useEffect(() => {
    clearSelection();
    setFocusedItem(null);
    setRenameFile(null);
    setRenameFolder(null);
    closePreview();
    closeDeleteModal();
    closeMoveModal();
  }, [
    config?.provider,
    config?.accountId,
    config?.bucket,
    currentPath,
    clearSelection,
    closePreview,
    closeDeleteModal,
    closeMoveModal,
  ]);

  const backgroundSync = useSyncStore((state) => state.backgroundSync);

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

    let cancelled = false;
    setSearchResults([]);
    setIsSearching(true);
    const timer = setTimeout(async () => {
      try {
        const result = await searchFiles(searchQuery);
        if (cancelled) return;
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
        if (cancelled) return;
        console.error('Search error:', e);
        setSearchResults([]);
        setSearchTotalCount(0);
      } finally {
        if (!cancelled) setIsSearching(false);
      }
    }, 300); // 300ms debounce

    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [searchQuery, isSynced, lastSyncTime, config?.provider, config?.accountId, config?.bucket]);

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
      message.error(i18nT('page.loadFailed', { error: error.message }));
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
          const { type } = event.payload;

          // Visual drag-over feedback
          if (type === 'over') {
            setIsDragOver(true);
            return;
          }
          if (type === 'leave') {
            setIsDragOver(false);
            return;
          }

          // Handle drop
          if (type === 'drop') {
            setIsDragOver(false);
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
          }
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

  // Ensure the download store owns the real-time task state even before the modal opens.
  useEffect(() => {
    setupGlobalDownloadListeners();
    void setupGlobalMoveListeners().then(() => loadAllActiveMoves());
    setupGlobalRenameListeners();
    setupGlobalMountListeners();
  }, []);

  // Refill the queue whenever a running download reaches a terminal state.
  useEffect(() => {
    let unlistenStatus: UnlistenFn | undefined;
    const setupListeners = async () => {
      unlistenStatus = await listen<{
        task_id: string;
        status: string;
        error: string | null;
      }>('download-status-changed', (event) => {
        const { status } = event.payload;
        if (status === 'completed' || status === 'failed' || status === 'cancelled') {
          setTimeout(() => {
            startDownloadQueueRef.current();
          }, 100);
        }
      });
    };

    setupListeners();

    return () => {
      unlistenStatus?.();
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

  const handleItemClick = useCallback(
    (item: FileItem) => {
      if (item.isFolder) {
        setCurrentPath(item.key);
        setSearchQuery('');
        clearSelection();
      } else {
        openPreview(item, filteredItems);
      }
    },
    [clearSelection, openPreview, filteredItems]
  );

  const handleColumnNavigate = useCallback(
    (path: string) => {
      setCurrentPath(path);
      clearSelection();
    },
    [setCurrentPath, clearSelection]
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

  const handleRefresh = useCallback(async () => {
    try {
      await refreshSync();
      await refresh();
      message.success(i18nT('page.refreshed'));
    } catch {
      message.error(i18nT('page.refreshFailed'));
    }
  }, [refresh, refreshSync, message]);

  const handleDelete = useCallback(
    async (item: FileItem) => {
      if (!config) return;
      try {
        await deleteObject(config, item.key);
        message.success(i18nT('page.deleted', { name: item.name }));
        await refresh();
      } catch (e) {
        console.error('Delete error:', e);
        message.error(
          i18nT('page.deleteFailed', {
            error: e instanceof Error ? e.message : i18nT('common.unknownError'),
          })
        );
      }
    },
    [config, message, refresh]
  );

  const handleFolderDelete = useCallback(
    async (item: FileItem) => {
      if (!config) return;
      try {
        message.loading({ content: i18nT('page.loadingFolder'), key: 'folder-delete' });
        const objects = await listAllObjectsUnderPrefix(config, item.key);
        message.destroy('folder-delete');

        if (objects.length === 0) {
          message.info(i18nT('page.folderEmpty'));
          return;
        }

        // Set file keys for batch delete modal
        const keys = new Set(objects.map((obj) => obj.key));
        openDeleteModal(keys);
      } catch (e) {
        message.destroy('folder-delete');
        console.error('Folder delete error:', e);
        message.error(
          i18nT('page.listFailed', {
            error: e instanceof Error ? e.message : i18nT('common.unknownError'),
          })
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
        message.loading({ content: i18nT('page.loadingFolder'), key: 'folder-download' });
        const objects = await listAllObjectsUnderPrefix(config, item.key);
        message.destroy('folder-download');

        if (objects.length === 0) {
          message.info(i18nT('page.folderEmpty'));
          return;
        }

        // Open folder picker dialog
        const folder = await invoke<string | null>('select_download_folder');
        if (!folder) return; // User cancelled

        // Queue all files in one backend call: single IPC round trip + one
        // DB transaction instead of one of each per file.
        const stamp = Date.now();
        const sessions = await invoke<DownloadSession[]>('create_download_tasks', {
          tasks: objects.map((obj) => ({
            task_id: `download-${stamp}-${obj.key}`,
            object_key: obj.key,
            file_name: obj.key.split('/').pop() || obj.key,
            file_size: obj.size || 0,
          })),
          localPath: folder,
          bucket: config.bucket,
          accountId: config.accountId,
        });
        addDownloadTasks(
          sessions.map((s) => ({
            id: s.id,
            key: s.object_key,
            fileName: s.file_name,
            fileSize: s.file_size,
            localPath: s.local_path,
          }))
        );

        // Start download queue via Rust backend
        setTimeout(() => startDownloadQueue(), 50);

        message.success(i18nT('page.queuedDownloads', { count: objects.length }));
      } catch (e) {
        message.destroy('folder-download');
        console.error('Folder download error:', e);
        message.error(
          i18nT('page.downloadFolderFailed', {
            error: e instanceof Error ? e.message : i18nT('common.unknownError'),
          })
        );
      }
    },
    [config, isConfigReady, message, addDownloadTasks, startDownloadQueue]
  );

  const openBatchDeleteConfirm = useCallback(async () => {
    if (selectedKeys.size === 0) return;

    // Snapshot the current selection
    const currentSelection = new Set(selectedKeys);

    // Check if any folders are selected
    const hasFolders = Array.from(currentSelection).some((key) => key.endsWith('/'));

    let finalKeys = currentSelection;
    if (hasFolders) {
      message.loading({ content: i18nT('page.preparing'), key: 'batch-prep' });
      finalKeys = await expandFolderKeys(currentSelection);
      message.destroy('batch-prep');

      if (finalKeys.size === 0) {
        message.info(i18nT('page.noDelete'));
        return;
      }
    }

    openDeleteModal(finalKeys);
  }, [selectedKeys, expandFolderKeys, message, openDeleteModal]);

  // Batch ops update the local cache incrementally (backend cache-updated /
  // paths-removed events), so completion only needs a cheap cache-backed
  // refetch — restarting a full bucket sync here made every batch op pay a
  // whole-bucket LIST walk.
  const handleBatchDeleteSuccess = useCallback(async () => {
    clearSelection();
    await refresh();
  }, [refresh, clearSelection]);

  const openBatchMoveModalHandler = useCallback(async () => {
    if (selectedKeys.size === 0) return;

    // Snapshot the current selection
    const currentSelection = new Set(selectedKeys);

    // Check if any folders are selected
    const hasFolders = Array.from(currentSelection).some((key) => key.endsWith('/'));

    let finalKeys = currentSelection;
    if (hasFolders) {
      message.loading({ content: i18nT('page.preparing'), key: 'batch-prep' });
      finalKeys = await expandFolderKeys(currentSelection);
      message.destroy('batch-prep');

      if (finalKeys.size === 0) {
        message.info(i18nT('page.noMove'));
        return;
      }
    }

    openMoveModal(finalKeys);
  }, [selectedKeys, expandFolderKeys, message, openMoveModal]);

  const handleBatchMoveSuccess = useCallback(async () => {
    clearSelection();
    await refresh();
  }, [refresh, clearSelection]);

  // Row-level "Move" action in list view: opens the same move modal as the
  // batch toolbar, scoped to just this file.
  const handleMoveClick = useCallback(
    (item: FileItem) => {
      if (item.isFolder) return;
      openMoveModal(new Set([item.key]));
    },
    [openMoveModal]
  );

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
        message.error(
          i18nT('page.downloadFailed', {
            error: e instanceof Error ? e.message : i18nT('common.unknownError'),
          })
        );
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
        message.loading({ content: i18nT('page.preparing'), key: 'batch-download-prep' });
        fileKeys = await expandFolderKeys(currentSelection);
        message.destroy('batch-download-prep');

        if (fileKeys.size === 0) {
          message.info(i18nT('page.noDownload'));
          return;
        }
      }

      // Filter out folders from the keys
      const filesToDownload = Array.from(fileKeys).filter((key) => !key.endsWith('/'));

      if (filesToDownload.length === 0) {
        message.info(i18nT('page.noDownload'));
        return;
      }

      // Open folder picker dialog
      const folder = await invoke<string | null>('select_download_folder');
      if (!folder) return; // User cancelled

      // Queue all files in one backend call (single IPC + one DB transaction).
      // Rust looks up file sizes from cache when file_size is 0.
      const sizeByKey = new Map(filteredItems.map((item) => [item.key, item.size ?? 0]));
      const stamp = Date.now();
      const sessions = await invoke<DownloadSession[]>('create_download_tasks', {
        tasks: filesToDownload.map((key) => ({
          task_id: `download-${stamp}-${key}`,
          object_key: key,
          file_name: key.split('/').pop() || key,
          file_size: sizeByKey.get(key) ?? 0,
        })),
        localPath: folder,
        bucket: config.bucket,
        accountId: config.accountId,
      });
      addDownloadTasks(
        sessions.map((s) => ({
          id: s.id,
          key: s.object_key,
          fileName: s.file_name,
          fileSize: s.file_size,
          localPath: s.local_path,
        }))
      );

      // Start download queue via Rust backend
      setTimeout(() => startDownloadQueue(), 50);

      message.success(i18nT('page.queuedDownloads', { count: filesToDownload.length }));
    } catch (e) {
      console.error('Batch download error:', e);
      message.error(
        i18nT('page.startDownloadsFailed', {
          error: e instanceof Error ? e.message : i18nT('common.unknownError'),
        })
      );
    }
  }, [
    selectedKeys,
    config,
    isConfigReady,
    message,
    expandFolderKeys,
    filteredItems,
    addDownloadTasks,
    startDownloadQueue,
  ]);

  // Command palette action handler
  const handleCmdAction = useCallback(
    (action: CommandAction) => {
      if (action.type === 'bucket') {
        const store = useAccountStore.getState();
        if (action.provider === 'r2' && action.tokenId != null) {
          store.selectR2Bucket(action.tokenId, action.bucket);
        } else if (action.provider === 'aws') {
          store.selectAwsBucket(action.accountId, action.bucket);
        } else if (action.provider === 'minio') {
          store.selectMinioBucket(action.accountId, action.bucket);
        } else if (action.provider === 'rustfs') {
          store.selectRustfsBucket(action.accountId, action.bucket);
        }
      } else if (action.type === 'open') {
        if (action.value === 'upload') setUploadModalOpen(true);
        else if (action.value === 'settings') {
          setSettingsTab('appearance');
          setSettingsOpen(true);
        } else if (action.value === 'report') {
          useReportStore.getState().open();
        } else if (action.value === 'failures') {
          useTransferErrorStore.getState().open();
        }
        // 'dock' — TransferDock auto-shows when tasks are running; no extra state needed
      } else if (action.type === 'refresh') {
        handleRefresh();
        pushToast(i18nT('page.refreshed'), 'success');
      } else if (action.type === 'theme') {
        toggleTheme();
      } else if (action.type === 'view') {
        cycleViewMode();
      } else if (action.type === 'path') {
        setCurrentPath(action.value);
      }
    },
    [handleRefresh, pushToast, toggleTheme, setCurrentPath, cycleViewMode]
  );

  // Global keyboard shortcuts (new shortcuts not covered by useKeyboardShortcuts)
  useGlobalShortcuts({
    openPalette: () => setPaletteOpen(true),
    closePalette: () => setPaletteOpen(false),
    paletteOpen,
    openUpload: () => setUploadModalOpen(true),
    refresh: handleRefresh,
    openSettings: () => {
      setSettingsTab('appearance');
      setSettingsOpen(true);
    },
    toggleView: cycleViewMode,
    viewMode,
  });

  // Keyboard shortcuts
  useKeyboardShortcuts(
    {
      onSelectAll: selectAll,
      onDelete: openBatchDeleteConfirm,
      onRefresh: handleRefresh,
      onEscape: () => {
        if (selectedKeys.size > 0) {
          clearSelection();
        }
      },
      onFocusSearch: () => {
        searchInputRef.current?.focus({ cursor: 'end' });
      },
    },
    !!config && isConfigReady
  );

  const handleRenameSuccess = useCallback(() => {
    if (previewFile?.key === renameFile?.key) {
      closePreview();
    }
    refresh();
  }, [renameFile, previewFile, closePreview, refresh]);

  const handleFolderRenameSuccess = useCallback(async () => {
    await refresh();
  }, [refresh]);

  // Refresh the file list whenever a rename batch finishes in the background
  // (the modal may have been closed while the batch kept running in the dock).
  const renameCompletedAt = useRenameStore((s) => s.lastCompletedAt);
  useEffect(() => {
    if (renameCompletedAt === 0) return;
    refresh();
  }, [renameCompletedAt, refresh]);

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

  useEffect(() => {
    if (initialized && !loading && performance.getEntriesByName('r2:shell-ready').length === 0) {
      // After the initialized shell commits and its React handlers are bound.
      // This local timing mark contains no account or storage data.
      performance.mark('r2:shell-ready');
    }
  }, [initialized, loading]);

  if (loading) {
    return (
      <div className="center-container">
        <Spin fullscreen />
      </div>
    );
  }

  const appBodyClass = [
    'app-body',
    sidebarStyle === 'collapsed' ? 'collapsed' : '',
    sidebarStyle === 'floating' ? 'floating' : '',
  ]
    .filter(Boolean)
    .join(' ');

  return (
    <div className="app-shell">
      <Titlebar onOpenPalette={() => setPaletteOpen(true)} />
      <div className={appBodyClass}>
        {/* Sidebar */}
        <AccountSidebar
          onAddAccount={handleAddAccount}
          onEditAccount={handleEditAccount}
          onAddToken={handleAddToken}
          onEditToken={handleEditToken}
          onOpenSettings={(tab) => {
            setSettingsTab(tab ?? 'account');
            setSettingsOpen(true);
          }}
        />

        {/* Main Content */}
        <div
          className={['main-content', isDragOver && 'drag-over'].filter(Boolean).join(' ')}
          ref={mainContentRef}
        >
          <div className="file-manager">
            {/* Toolbar */}
            <Toolbar
              searchQuery={searchQuery}
              onSearchChange={setSearchQuery}
              viewMode={viewMode}
              onViewModeChange={setViewMode}
              onRefresh={handleRefresh}
              isRefreshing={isFetching}
              onUploadOpen={() => setUploadModalOpen(true)}
              onSettingsOpen={() => {
                setSettingsTab('account');
                setSettingsOpen(true);
              }}
              onAppearanceOpen={() => {
                setSettingsTab('appearance');
                setSettingsOpen(true);
              }}
              bucketName={currentConfig?.bucket ?? null}
              bucketSize={null}
              bucketCount={null}
              onNavigate={(newPath) => {
                setCurrentPath(newPath);
                setSearchQuery('');
              }}
            />

            {/* Selection action bar */}
            {selectedKeys.size > 0 && (
              <SelectionActionBar
                selectedCount={selectedKeys.size}
                fileCount={selectedFileCount !== selectedKeys.size ? selectedFileCount : undefined}
                onDownload={handleBatchDownload}
                onMove={openBatchMoveModalHandler}
                onDelete={openBatchDeleteConfirm}
                onClear={clearSelection}
              />
            )}

            {/* Floating sync progress banner — auto-shows at bottom-left
                whenever any sync pipeline is running. Self-hides when idle. */}
            <SyncBanner />

            {/* Partial-data search indicator during background sync */}
            {searchQuery.trim() && backgroundSync.isRunning && (
              <div
                style={{
                  padding: '4px 12px',
                  fontSize: 12,
                  color: 'var(--ant-color-text-secondary)',
                  background: 'var(--ant-color-fill-quaternary)',
                  borderRadius: 4,
                  margin: '0 16px 4px',
                }}
              >
                Searching {backgroundSync.objectsFetched.toLocaleString()}
                {backgroundSync.estimatedTotal
                  ? ` of ~${backgroundSync.estimatedTotal.toLocaleString()}`
                  : ''}{' '}
                objects (sync in progress)
              </div>
            )}

            {hasData && !searchQuery.trim() && (error || isPartial || (isCached && isFetching)) && (
              <Alert
                type={error ? 'warning' : 'info'}
                showIcon
                title={
                  error
                    ? isPartial
                      ? t('page.partialRefreshFailed')
                      : t('page.cachedRefreshFailed')
                    : isPartial
                      ? t('page.loadingMore')
                      : t('page.cachedChecking')
                }
                description={error?.message}
                action={
                  error ? (
                    <Button onClick={() => void refresh()}>{t('page.retry')}</Button>
                  ) : undefined
                }
              />
            )}

            {/* File area */}
            <div className="file-area" style={{ flex: 1, minHeight: 0, position: 'relative' }}>
              {!config ? (
                <EmptyState onUpload={() => setUploadModalOpen(true)} />
              ) : error && !hasData ? (
                <Alert
                  type="error"
                  showIcon
                  title={t('page.folderLoadFailed')}
                  description={error.message}
                  action={<Button onClick={() => void refresh()}>{t('page.retry')}</Button>}
                />
              ) : viewMode === 'column' && !searchQuery.trim() ? (
                // Column view shows the folder trail even while the current
                // folder loads or is empty; search results stay a flat list.
                <FileColumnView
                  key={`${config.provider}:${config.accountId}:${config.bucket}`}
                  config={config}
                  currentPath={currentPath}
                  currentItems={items}
                  currentLoading={isLoading}
                  selectedKeys={selectedKeys}
                  onNavigate={handleColumnNavigate}
                  onOpen={openPreview}
                  onToggleSelection={toggleSelection}
                  onDelete={handleDelete}
                  onRename={handleRenameClick}
                  onDownload={handleDownload}
                  onFolderDelete={handleFolderDelete}
                  onFolderDownload={handleFolderDownload}
                  onFolderRename={handleFolderRenameClick}
                />
              ) : isLoading || isSearching ? (
                <div className="file-list-loading">
                  <Spin description={isSearching ? t('page.searching') : undefined} fullscreen />
                </div>
              ) : filteredItems.length === 0 ? (
                <EmptyState
                  search={searchQuery || undefined}
                  onUpload={() => setUploadModalOpen(true)}
                />
              ) : viewMode !== 'grid' ? (
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
                  onMove={handleMoveClick}
                  onFocus={setFocusedItem}
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
                  onFocus={setFocusedItem}
                />
              )}

              {/* Inspector right-rail */}
              {showInspector && focusedItem && (
                <Inspector
                  item={focusedItem}
                  bucket={currentConfig?.bucket ?? ''}
                  path={currentPath}
                  storageConfig={config}
                  onClose={() => setShowInspector(false)}
                  onDownload={handleDownload}
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
              selectedCount={selectedKeys.size}
            />

            {configModalOpen && (
              <ConfigModal
                open={true}
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
            )}

            {settingsOpen && (
              <SettingsModal
                open={true}
                onClose={() => setSettingsOpen(false)}
                initialTab={settingsTab}
              />
            )}

            {mountModalOpen && <MountModal />}

            {uploadModalOpen && (
              <UploadModal
                open={true}
                onClose={() => setUploadModalOpen(false)}
                currentPath={currentPath}
                config={config}
                dropQueue={dropQueue}
                onDropHandled={handleDropHandled}
                onUploadComplete={() => {
                  refresh();
                }}
                onCredentialsUpdate={() => {
                  initialize();
                }}
              />
            )}

            {previewFile && (
              <FilePreviewModal
                config={config}
                onCredentialsUpdate={() => {
                  initialize();
                }}
                onFileUpdated={() => {
                  void refresh();
                }}
              />
            )}

            {renameFile && config && (
              <FileRenameModal
                open={true}
                onClose={() => setRenameFile(null)}
                file={renameFile}
                config={config}
                onSuccess={handleRenameSuccess}
              />
            )}

            {renameFolder && (
              <FolderRenameModal
                open={true}
                onClose={() => setRenameFolder(null)}
                folder={renameFolder}
                folderMetadata={metadata[renameFolder.key]}
                config={config}
                onSuccess={handleFolderRenameSuccess}
              />
            )}

            {deleteModalOpen && (
              <BatchDeleteModal
                open={true}
                selectedKeys={keysToDelete}
                config={config}
                onClose={closeDeleteModal}
                onSuccess={handleBatchDeleteSuccess}
                onDeletingChange={setDeleting}
              />
            )}

            {moveModalOpen && (
              <BatchMoveModal
                open={true}
                selectedKeys={keysToMove}
                config={config}
                onClose={closeMoveModal}
                onSuccess={handleBatchMoveSuccess}
                onMovingChange={setMoving}
              />
            )}

            {moveTaskModalOpen && <MoveTaskModal storageConfig={config} />}
            {downloadTaskModalOpen && <DownloadTaskModal storageConfig={config} />}
          </div>
        </div>
      </div>

      {/* Report a problem — portal to body, opened from the status bar or the palette */}
      {reportOpen && <ReportProblemModal />}

      {/* Transfer failures — portal to body. After the batch modals and the report
          dialog, so it stacks above a delete modal that reports into it */}
      {transferErrorOpen && <TransferErrorModal />}

      {/* Command palette — portal to body */}
      {paletteOpen && (
        <CommandPalette
          open={true}
          onClose={() => setPaletteOpen(false)}
          onAction={handleCmdAction}
        />
      )}

      {/* Transfer dock — auto-shows when tasks are running */}
      <TransferDock />

      {/* Toast notification stack */}
      <Toast />
    </div>
  );
}
