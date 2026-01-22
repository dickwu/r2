import { invoke } from '@tauri-apps/api/core';
import type {
  BatchDeleteResult,
  BatchMoveResult,
  ListObjectsOptions,
  ListObjectsResult,
  RustfsStorageConfig,
  StorageBucket,
  StorageObject,
  SyncResult,
  MoveOperation,
  UploadFileInput,
  UploadFileResult,
  StorageProviderAdapter,
} from '@/app/providers/types';

function requireRustfsFields(config: RustfsStorageConfig) {
  if (!config.accessKeyId || !config.secretAccessKey || !config.endpointHost) {
    throw new Error('Missing RustFS credentials or endpoint');
  }
}

function toRustfsConfigInput(config: RustfsStorageConfig) {
  requireRustfsFields(config);
  return {
    account_id: config.accountId,
    bucket: config.bucket,
    access_key_id: config.accessKeyId,
    secret_access_key: config.secretAccessKey,
    endpoint_scheme: config.endpointScheme,
    endpoint_host: config.endpointHost,
    force_path_style: true,
  };
}

export const rustfsProvider: StorageProviderAdapter<RustfsStorageConfig> = {
  listBuckets: async (config) => {
    requireRustfsFields(config);
    return invoke<StorageBucket[]>('list_rustfs_buckets', {
      accountId: config.accountId,
      accessKeyId: config.accessKeyId,
      secretAccessKey: config.secretAccessKey,
      endpointScheme: config.endpointScheme,
      endpointHost: config.endpointHost,
      forcePathStyle: true,
    });
  },

  listObjects: async (config, options: ListObjectsOptions = {}) => {
    const { prefix, delimiter, cursor, perPage } = options;
    return invoke<ListObjectsResult>('list_rustfs_objects', {
      input: {
        config: toRustfsConfigInput(config),
        prefix: prefix || null,
        delimiter: delimiter || null,
        continuation_token: cursor || null,
        max_keys: perPage || null,
      },
    });
  },

  listAllObjectsRecursive: async (config) => {
    return invoke<StorageObject[]>('list_all_rustfs_objects', {
      config: toRustfsConfigInput(config),
    });
  },

  listFolderObjects: async (config, prefix) => {
    return invoke<ListObjectsResult>('list_folder_rustfs_objects', {
      config: toRustfsConfigInput(config),
      prefix: prefix || null,
    });
  },

  deleteObject: async (config, key) => {
    return invoke('delete_rustfs_object', {
      config: toRustfsConfigInput(config),
      key,
    });
  },

  batchDeleteObjects: async (config, keys) => {
    return invoke<BatchDeleteResult>('batch_delete_rustfs_objects', {
      config: toRustfsConfigInput(config),
      keys,
    });
  },

  renameObject: async (config, oldKey, newKey) => {
    return invoke('rename_rustfs_object', {
      config: toRustfsConfigInput(config),
      oldKey,
      newKey,
    });
  },

  batchMoveObjects: async (config, operations: MoveOperation[]) => {
    return invoke<BatchMoveResult>('batch_move_rustfs_objects', {
      config: toRustfsConfigInput(config),
      operations,
    });
  },

  generateSignedUrl: async (config, key, expiresIn) => {
    return invoke<string>('generate_rustfs_signed_url', {
      config: toRustfsConfigInput(config),
      key,
      expiresIn,
    });
  },

  uploadFile: async (config, input: UploadFileInput) => {
    requireRustfsFields(config);
    return invoke<UploadFileResult>('upload_rustfs_file', {
      taskId: input.taskId,
      filePath: input.filePath,
      key: input.key,
      contentType: input.contentType ?? null,
      accountId: config.accountId,
      bucket: config.bucket,
      accessKeyId: config.accessKeyId,
      secretAccessKey: config.secretAccessKey,
      endpointScheme: config.endpointScheme,
      endpointHost: config.endpointHost,
      forcePathStyle: true,
    });
  },

  uploadContent: async (config, key, content, contentType) => {
    return invoke<string>('upload_rustfs_content', {
      config: toRustfsConfigInput(config),
      key,
      content,
      contentType,
    });
  },

  syncBucket: async (config) => {
    return invoke<SyncResult>('sync_rustfs_bucket', {
      config: toRustfsConfigInput(config),
    });
  },

  buildBucketBaseUrl: (config) => {
    if (!config.bucket || !config.endpointHost) return null;
    const scheme = config.endpointScheme || 'https';
    const host = config.endpointHost;
    return `${scheme}://${host}/${config.bucket}`;
  },

  buildPublicUrl: (config, key) => {
    const base = rustfsProvider.buildBucketBaseUrl(config);
    if (!base) return null;
    return `${base}/${encodeURI(key)}`;
  },
};
