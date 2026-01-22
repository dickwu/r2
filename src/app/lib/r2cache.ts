import { invoke } from '@tauri-apps/api/core';
import { getProviderAdapter } from '@/app/providers/index';
import type {
  BatchDeleteResult,
  BatchMoveResult,
  ListObjectsOptions,
  ListObjectsResult,
  MoveOperation,
  StorageBucket,
  StorageConfig,
  StorageObject,
  SyncResult,
  UploadFileInput,
  UploadFileResult,
} from '@/app/providers/types';

export type {
  BatchDeleteResult,
  BatchMoveResult,
  ListObjectsOptions,
  ListObjectsResult,
  MoveOperation,
  StorageBucket,
  StorageConfig,
  StorageObject,
  SyncResult,
  StorageProvider,
  R2StorageConfig,
  AwsStorageConfig,
  MinioStorageConfig,
  RustfsStorageConfig,
  UploadFileInput,
  UploadFileResult,
} from '@/app/providers/types';

export interface StoredFile {
  key: string;
  size: number;
  lastModified: string;
}

export interface DirectoryNode {
  path: string;
  fileCount: number;
  totalFileCount: number;
  size: number;
  totalSize: number;
  lastModified: string | null;
  lastUpdated: number;
}

// ============ Provider Operations ============

export async function listBuckets(config: StorageConfig): Promise<StorageBucket[]> {
  return getProviderAdapter(config).listBuckets(config);
}

export async function listObjects(
  config: StorageConfig,
  options: ListObjectsOptions = {}
): Promise<ListObjectsResult> {
  return getProviderAdapter(config).listObjects(config, options);
}

export async function listAllObjects(
  config: StorageConfig,
  prefix: string = ''
): Promise<ListObjectsResult> {
  return getProviderAdapter(config).listFolderObjects(config, prefix);
}

export async function listAllObjectsRecursive(config: StorageConfig): Promise<StorageObject[]> {
  return getProviderAdapter(config).listAllObjectsRecursive(config);
}

export async function listAllObjectsUnderPrefix(
  config: StorageConfig,
  prefix: string
): Promise<StorageObject[]> {
  const allObjects: StorageObject[] = [];
  let cursor: string | undefined;

  do {
    // No delimiter = recursive listing under prefix
    const result = await listObjects(config, { prefix, cursor });
    allObjects.push(...result.objects);
    cursor = result.truncated ? result.continuation_token : undefined;
  } while (cursor);

  return allObjects;
}

export async function deleteObject(config: StorageConfig, key: string): Promise<void> {
  return getProviderAdapter(config).deleteObject(config, key);
}

export async function batchDeleteObjects(
  config: StorageConfig,
  keys: string[]
): Promise<BatchDeleteResult> {
  return getProviderAdapter(config).batchDeleteObjects(config, keys);
}

export async function renameObject(
  config: StorageConfig,
  oldKey: string,
  newKey: string
): Promise<void> {
  return getProviderAdapter(config).renameObject(config, oldKey, newKey);
}

export async function batchMoveObjects(
  config: StorageConfig,
  operations: MoveOperation[]
): Promise<BatchMoveResult> {
  return getProviderAdapter(config).batchMoveObjects(config, operations);
}

export async function generateSignedUrl(
  config: StorageConfig,
  key: string,
  expiresIn: number = 3600
): Promise<string> {
  return getProviderAdapter(config).generateSignedUrl(config, key, expiresIn);
}

export async function uploadFile(
  config: StorageConfig,
  input: UploadFileInput
): Promise<UploadFileResult> {
  return getProviderAdapter(config).uploadFile(config, input);
}

export async function uploadContent(
  config: StorageConfig,
  key: string,
  content: string,
  contentType?: string
): Promise<string> {
  return getProviderAdapter(config).uploadContent(config, key, content, contentType);
}

// ============ Sync Operation (consolidated) ============

export async function syncBucket(config: StorageConfig): Promise<SyncResult> {
  return getProviderAdapter(config).syncBucket(config);
}

export function buildBucketBaseUrl(config: StorageConfig): string | null {
  return getProviderAdapter(config).buildBucketBaseUrl(config);
}

export function buildPublicUrl(config: StorageConfig, key: string): string | null {
  return getProviderAdapter(config).buildPublicUrl(config, key);
}

// ============ Folder Contents (from cache) ============

export interface FolderContents {
  files: StoredFile[];
  folders: string[];
}

export async function getFolderContents(prefix: string = ''): Promise<FolderContents> {
  return invoke('get_folder_contents', { prefix: prefix || null });
}

// ============ Cache Operations (replaces IndexedDB) ============

/** @deprecated Use syncBucket() instead - files are now stored during sync */
export async function storeAllFiles(files: StorageObject[]): Promise<void> {
  return invoke('store_all_files', { files });
}

export async function getAllFiles(): Promise<StoredFile[]> {
  return invoke('get_all_cached_files');
}

export interface SearchResult {
  files: StoredFile[];
  totalCount: number;
}

export async function searchFiles(query: string): Promise<SearchResult> {
  return invoke('search_cached_files', { query });
}

export async function calculateFolderSize(folderPrefix: string): Promise<number> {
  return invoke('calculate_folder_size', { prefix: folderPrefix });
}

/** @deprecated Use syncBucket() instead - directory tree is now built during sync */
export async function buildDirectoryTree(files: StoredFile[]): Promise<void> {
  // Files are already in DB, just need to build the tree
  return invoke('build_directory_tree');
}

export async function getDirectoryNode(path: string): Promise<DirectoryNode | null> {
  return invoke('get_directory_node', { path });
}

export async function getAllDirectoryNodes(): Promise<DirectoryNode[]> {
  return invoke('get_all_directory_nodes');
}

export async function clearCache(): Promise<void> {
  return invoke('clear_file_cache');
}

// Note: Upload state functions removed - now handled by backend upload_sessions table
// The following functions from indexeddb.ts are NOT migrated:
// - generateUploadStateId
// - getUploadState
// - saveUploadState
// - deleteUploadState
// - getAllUploadStates
// - clearOldUploadStates
// These are handled directly by the backend via upload.rs and db.rs
