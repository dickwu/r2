import { invoke } from '@tauri-apps/api/core';

// ============ Types ============

export interface R2Object {
  key: string;
  size: number;
  last_modified: string;
  etag: string;
}

export interface R2Bucket {
  name: string;
  creation_date: string;
}

export interface ListObjectsResult {
  objects: R2Object[];
  folders: string[];
  truncated: boolean;
  continuation_token?: string;
}

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

export interface R2Config {
  accountId: string;
  token?: string;
  accessKeyId?: string;
  secretAccessKey?: string;
  bucket: string;
  publicDomain?: string;
}

// ============ R2 Operations ============

export async function listR2Buckets(
  accountId: string,
  accessKeyId: string,
  secretAccessKey: string
): Promise<R2Bucket[]> {
  return invoke('list_r2_buckets', {
    accountId,
    accessKeyId,
    secretAccessKey,
  });
}

export interface ListObjectsOptions {
  prefix?: string;
  delimiter?: string;
  cursor?: string;
  perPage?: number;
}

export async function listR2Objects(
  config: R2Config,
  options: ListObjectsOptions = {}
): Promise<ListObjectsResult> {
  const { prefix, delimiter, cursor, perPage } = options;

  return invoke('list_r2_objects', {
    input: {
      config: {
        account_id: config.accountId,
        bucket: config.bucket,
        access_key_id: config.accessKeyId || '',
        secret_access_key: config.secretAccessKey || '',
      },
      prefix: prefix || null,
      delimiter: delimiter || null,
      continuation_token: cursor || null,
      max_keys: perPage || null,
    },
  });
}

export async function listAllR2Objects(
  config: R2Config,
  prefix: string = ''
): Promise<ListObjectsResult> {
  const allObjects: R2Object[] = [];
  const allFolders: string[] = [];
  let cursor: string | undefined;

  do {
    const result = await listR2Objects(config, { prefix, cursor, delimiter: '/' });
    allObjects.push(...result.objects);

    // Only add unique folders
    for (const folder of result.folders) {
      if (!allFolders.includes(folder)) {
        allFolders.push(folder);
      }
    }

    cursor = result.truncated ? result.continuation_token : undefined;
  } while (cursor);

  return {
    objects: allObjects,
    folders: allFolders,
    truncated: false,
  };
}

export async function listAllR2ObjectsRecursive(config: R2Config): Promise<R2Object[]> {
  // Progress is now emitted via Tauri events ('sync-progress')
  // Listen to these events in your component to track progress
  return invoke('list_all_r2_objects', {
    config: {
      account_id: config.accountId,
      bucket: config.bucket,
      access_key_id: config.accessKeyId || '',
      secret_access_key: config.secretAccessKey || '',
    },
  });
}

export async function deleteR2Object(config: R2Config, key: string): Promise<void> {
  return invoke('delete_r2_object', {
    config: {
      account_id: config.accountId,
      bucket: config.bucket,
      access_key_id: config.accessKeyId || '',
      secret_access_key: config.secretAccessKey || '',
    },
    key,
  });
}

export async function renameR2Object(
  config: R2Config,
  oldKey: string,
  newKey: string
): Promise<void> {
  return invoke('rename_r2_object', {
    config: {
      account_id: config.accountId,
      bucket: config.bucket,
      access_key_id: config.accessKeyId || '',
      secret_access_key: config.secretAccessKey || '',
    },
    oldKey,
    newKey,
  });
}

export async function generateSignedUrl(
  accountId: string,
  bucket: string,
  key: string,
  accessKeyId: string,
  secretAccessKey: string,
  expiresIn: number = 3600
): Promise<string> {
  return invoke('generate_signed_url', {
    config: {
      account_id: accountId,
      bucket,
      access_key_id: accessKeyId,
      secret_access_key: secretAccessKey,
    },
    key,
    expiresIn,
  });
}

// ============ Cache Operations (replaces IndexedDB) ============

export async function storeAllFiles(files: R2Object[]): Promise<void> {
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
