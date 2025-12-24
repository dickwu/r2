import { R2Object } from './r2api';

const DB_NAME = 'r2-uploader';
const DB_VERSION = 4;
const FILES_STORE = 'files';
const META_STORE = 'meta';
const UPLOADS_STORE = 'uploads';
const DIRECTORY_TREE_STORE = 'directory_tree';

export interface StoredFile {
  key: string;
  size: number;
  lastModified: string;
}

export interface DirectoryNode {
  path: string; // Directory path (e.g., "folder1/" or "folder1/subfolder/")
  fileCount: number; // Number of files directly in this directory
  totalFileCount: number; // Total files including subdirectories
  size: number; // Total size of files directly in this directory
  totalSize: number; // Total size including subdirectories
  lastUpdated: number; // Timestamp of last calculation
}

// Resumable upload state
export interface UploadState {
  id: string; // Unique ID: hash of file identity + destination
  uploadId: string; // S3 multipart upload ID
  bucket: string;
  accountId: string;
  key: string; // Destination path
  fileName: string;
  fileSize: number;
  fileLastModified: number;
  contentType: string;
  totalParts: number;
  completedParts: { PartNumber: number; ETag: string }[];
  createdAt: number;
  updatedAt: number;
}

let dbPromise: Promise<IDBDatabase> | null = null;

function openDB(): Promise<IDBDatabase> {
  if (dbPromise) return dbPromise;

  dbPromise = new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, DB_VERSION);

    request.onerror = () => reject(request.error);
    request.onsuccess = () => resolve(request.result);

    request.onupgradeneeded = (event) => {
      const db = (event.target as IDBOpenDBRequest).result;

      // Delete old stores if upgrading
      if (db.objectStoreNames.contains('files')) {
        db.deleteObjectStore('files');
      }

      // Files store: flat list of all files, keyed by full path
      const filesStore = db.createObjectStore(FILES_STORE, { keyPath: 'key' });
      filesStore.createIndex('key', 'key', { unique: true });

      // Meta store: for tracking sync state
      if (!db.objectStoreNames.contains(META_STORE)) {
        db.createObjectStore(META_STORE, { keyPath: 'id' });
      }

      // Uploads store: for resumable uploads
      if (!db.objectStoreNames.contains(UPLOADS_STORE)) {
        db.createObjectStore(UPLOADS_STORE, { keyPath: 'id' });
      }

      // Directory tree store: for aggregated directory metadata
      if (!db.objectStoreNames.contains(DIRECTORY_TREE_STORE)) {
        const treeStore = db.createObjectStore(DIRECTORY_TREE_STORE, { keyPath: 'path' });
        treeStore.createIndex('path', 'path', { unique: true });
      }
    };
  });

  return dbPromise;
}

// Store all files from a bucket (replaces existing data)
export async function storeAllFiles(files: R2Object[]): Promise<void> {
  const db = await openDB();
  const tx = db.transaction(FILES_STORE, 'readwrite');
  const store = tx.objectStore(FILES_STORE);

  // Clear existing data
  store.clear();

  // Add all files
  for (const file of files) {
    if (!file.key.endsWith('/')) {
      store.put({
        key: file.key,
        size: file.size,
        lastModified: file.last_modified,
      } as StoredFile);
    }
  }

  return new Promise((resolve, reject) => {
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
  });
}

// Get all files (for full list)
export async function getAllFiles(): Promise<StoredFile[]> {
  const db = await openDB();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(FILES_STORE, 'readonly');
    const store = tx.objectStore(FILES_STORE);
    const request = store.getAll();
    request.onsuccess = () => resolve(request.result ?? []);
    request.onerror = () => reject(request.error);
  });
}

// Calculate folder size from IndexedDB (all files with prefix)
export async function calculateFolderSize(folderPrefix: string): Promise<number> {
  const db = await openDB();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(FILES_STORE, 'readonly');
    const store = tx.objectStore(FILES_STORE);
    const request = store.openCursor();

    let totalSize = 0;

    request.onsuccess = (event) => {
      const cursor = (event.target as IDBRequest<IDBCursorWithValue>).result;
      if (cursor) {
        const file = cursor.value as StoredFile;
        if (file.key.startsWith(folderPrefix)) {
          totalSize += file.size;
        }
        cursor.continue();
      } else {
        resolve(totalSize);
      }
    };

    request.onerror = () => reject(request.error);
  });
}

// Build directory tree from all files
export async function buildDirectoryTree(files: StoredFile[]): Promise<void> {
  const db = await openDB();
  const tx = db.transaction(DIRECTORY_TREE_STORE, 'readwrite');
  const store = tx.objectStore(DIRECTORY_TREE_STORE);

  // Clear existing tree
  store.clear();

  // Build directory map
  const dirMap = new Map<string, { files: StoredFile[]; subdirs: Set<string> }>();

  // Initialize root directory
  dirMap.set('', { files: [], subdirs: new Set() });

  // Extract all unique directories from file paths
  for (const file of files) {
    const parts = file.key.split('/');

    // Handle root-level files (no directory)
    if (parts.length === 1) {
      dirMap.get('')!.files.push(file);
      continue;
    }

    // Traverse each directory level
    for (let i = 0; i < parts.length - 1; i++) {
      // Build path from root to current level
      const currentPath = parts.slice(0, i + 1).join('/') + '/';
      const prevPath = i > 0 ? parts.slice(0, i).join('/') + '/' : '';

      if (!dirMap.has(currentPath)) {
        dirMap.set(currentPath, { files: [], subdirs: new Set() });
      }

      // Track parent-child relationship with root or parent directory
      const parentDir = prevPath || '';
      if (dirMap.has(parentDir)) {
        dirMap.get(parentDir)!.subdirs.add(currentPath);
      }

      // Add file to its direct parent directory
      if (i === parts.length - 2) {
        dirMap.get(currentPath)!.files.push(file);
      }
    }
  }

  // Calculate sizes and counts (bottom-up)
  const nodes: DirectoryNode[] = [];
  const sortedDirs = Array.from(dirMap.keys()).sort(
    (a, b) => b.split('/').length - a.split('/').length
  );

  const nodeMap = new Map<string, DirectoryNode>();

  for (const path of sortedDirs) {
    const data = dirMap.get(path)!;

    // Direct files in this directory
    const directSize = data.files.reduce((sum, f) => sum + f.size, 0);
    const directCount = data.files.length;

    // Aggregate from subdirectories
    let subSize = 0;
    let subCount = 0;
    for (const subdir of data.subdirs) {
      const subNode = nodeMap.get(subdir);
      if (subNode) {
        subSize += subNode.totalSize;
        subCount += subNode.totalFileCount;
      }
    }

    const node: DirectoryNode = {
      path,
      fileCount: directCount,
      totalFileCount: directCount + subCount,
      size: directSize,
      totalSize: directSize + subSize,
      lastUpdated: Date.now(),
    };

    nodeMap.set(path, node);
    nodes.push(node);
  }

  // Store all nodes
  for (const node of nodes) {
    store.put(node);
  }

  return new Promise((resolve, reject) => {
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
  });
}

// Get directory node by path
export async function getDirectoryNode(path: string): Promise<DirectoryNode | null> {
  const db = await openDB();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(DIRECTORY_TREE_STORE, 'readonly');
    const store = tx.objectStore(DIRECTORY_TREE_STORE);
    const request = store.get(path);
    request.onsuccess = () => resolve(request.result ?? null);
    request.onerror = () => reject(request.error);
  });
}

// Get all directory nodes
export async function getAllDirectoryNodes(): Promise<DirectoryNode[]> {
  const db = await openDB();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(DIRECTORY_TREE_STORE, 'readonly');
    const store = tx.objectStore(DIRECTORY_TREE_STORE);
    const request = store.getAll();
    request.onsuccess = () => resolve(request.result ?? []);
    request.onerror = () => reject(request.error);
  });
}

// Clear all cached data
export async function clearCache(): Promise<void> {
  const db = await openDB();
  const tx = db.transaction([FILES_STORE, META_STORE, DIRECTORY_TREE_STORE], 'readwrite');
  tx.objectStore(FILES_STORE).clear();
  tx.objectStore(META_STORE).clear();
  tx.objectStore(DIRECTORY_TREE_STORE).clear();
  return new Promise((resolve, reject) => {
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
  });
}

// ============ Resumable Upload Functions ============

// Generate unique ID for upload state
export function generateUploadStateId(
  accountId: string,
  bucket: string,
  key: string,
  fileName: string,
  fileSize: number,
  fileLastModified: number
): string {
  return `${accountId}:${bucket}:${key}:${fileName}:${fileSize}:${fileLastModified}`;
}

// Get upload state by ID
export async function getUploadState(id: string): Promise<UploadState | null> {
  const db = await openDB();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(UPLOADS_STORE, 'readonly');
    const store = tx.objectStore(UPLOADS_STORE);
    const request = store.get(id);
    request.onsuccess = () => resolve(request.result ?? null);
    request.onerror = () => reject(request.error);
  });
}

// Save upload state
export async function saveUploadState(state: UploadState): Promise<void> {
  const db = await openDB();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(UPLOADS_STORE, 'readwrite');
    const store = tx.objectStore(UPLOADS_STORE);
    store.put({ ...state, updatedAt: Date.now() });
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
  });
}

// Delete upload state
export async function deleteUploadState(id: string): Promise<void> {
  const db = await openDB();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(UPLOADS_STORE, 'readwrite');
    const store = tx.objectStore(UPLOADS_STORE);
    store.delete(id);
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
  });
}

// Get all pending uploads (for UI display)
export async function getAllUploadStates(): Promise<UploadState[]> {
  const db = await openDB();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(UPLOADS_STORE, 'readonly');
    const store = tx.objectStore(UPLOADS_STORE);
    const request = store.getAll();
    request.onsuccess = () => resolve(request.result ?? []);
    request.onerror = () => reject(request.error);
  });
}

// Clear old upload states (older than 7 days)
export async function clearOldUploadStates(): Promise<void> {
  const db = await openDB();
  const sevenDaysAgo = Date.now() - 7 * 24 * 60 * 60 * 1000;

  return new Promise((resolve, reject) => {
    const tx = db.transaction(UPLOADS_STORE, 'readwrite');
    const store = tx.objectStore(UPLOADS_STORE);
    const request = store.openCursor();

    request.onsuccess = (event) => {
      const cursor = (event.target as IDBRequest<IDBCursorWithValue>).result;
      if (cursor) {
        const state = cursor.value as UploadState;
        if (state.updatedAt < sevenDaysAgo) {
          cursor.delete();
        }
        cursor.continue();
      }
    };

    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
  });
}
