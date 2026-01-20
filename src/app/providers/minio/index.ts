import { invoke } from '@tauri-apps/api/core';
import type {
  BatchDeleteResult,
  BatchMoveResult,
  ListObjectsOptions,
  ListObjectsResult,
  MinioStorageConfig,
  StorageBucket,
  StorageObject,
  SyncResult,
  MoveOperation,
  StorageProviderAdapter,
} from '../types';

function requireMinioFields(config: MinioStorageConfig) {
  if (!config.accessKeyId || !config.secretAccessKey || !config.endpointHost) {
    throw new Error('Missing MinIO credentials or endpoint');
  }
}

function toMinioConfigInput(config: MinioStorageConfig) {
  requireMinioFields(config);
  return {
    account_id: config.accountId,
    bucket: config.bucket,
    access_key_id: config.accessKeyId,
    secret_access_key: config.secretAccessKey,
    endpoint_scheme: config.endpointScheme,
    endpoint_host: config.endpointHost,
    force_path_style: config.forcePathStyle,
  };
}

export const minioProvider: StorageProviderAdapter<MinioStorageConfig> = {
  listBuckets: async (config) => {
    requireMinioFields(config);
    return invoke<StorageBucket[]>('list_minio_buckets', {
      accountId: config.accountId,
      accessKeyId: config.accessKeyId,
      secretAccessKey: config.secretAccessKey,
      endpointScheme: config.endpointScheme,
      endpointHost: config.endpointHost,
      forcePathStyle: config.forcePathStyle,
    });
  },

  listObjects: async (config, options: ListObjectsOptions = {}) => {
    const { prefix, delimiter, cursor, perPage } = options;
    return invoke<ListObjectsResult>('list_minio_objects', {
      input: {
        config: toMinioConfigInput(config),
        prefix: prefix || null,
        delimiter: delimiter || null,
        continuation_token: cursor || null,
        max_keys: perPage || null,
      },
    });
  },

  listAllObjectsRecursive: async (config) => {
    return invoke<StorageObject[]>('list_all_minio_objects', {
      config: toMinioConfigInput(config),
    });
  },

  listFolderObjects: async (config, prefix) => {
    return invoke<ListObjectsResult>('list_folder_minio_objects', {
      config: toMinioConfigInput(config),
      prefix: prefix || null,
    });
  },

  deleteObject: async (config, key) => {
    return invoke('delete_minio_object', {
      config: toMinioConfigInput(config),
      key,
    });
  },

  batchDeleteObjects: async (config, keys) => {
    return invoke<BatchDeleteResult>('batch_delete_minio_objects', {
      config: toMinioConfigInput(config),
      keys,
    });
  },

  renameObject: async (config, oldKey, newKey) => {
    return invoke('rename_minio_object', {
      config: toMinioConfigInput(config),
      oldKey,
      newKey,
    });
  },

  batchMoveObjects: async (config, operations: MoveOperation[]) => {
    return invoke<BatchMoveResult>('batch_move_minio_objects', {
      config: toMinioConfigInput(config),
      operations,
    });
  },

  generateSignedUrl: async (config, key, expiresIn) => {
    return invoke<string>('generate_minio_signed_url', {
      config: toMinioConfigInput(config),
      key,
      expiresIn,
    });
  },

  uploadContent: async (config, key, content, contentType) => {
    return invoke<string>('upload_minio_content', {
      config: toMinioConfigInput(config),
      key,
      content,
      contentType,
    });
  },

  syncBucket: async (config) => {
    return invoke<SyncResult>('sync_minio_bucket', {
      config: toMinioConfigInput(config),
    });
  },

  buildBucketBaseUrl: (config) => {
    if (!config.bucket || !config.endpointHost) return null;
    const scheme = config.endpointScheme || 'https';
    const host = config.endpointHost;
    if (config.forcePathStyle) {
      return `${scheme}://${host}/${config.bucket}`;
    }
    return `${scheme}://${config.bucket}.${host}`;
  },

  buildPublicUrl: (config, key) => {
    const base = minioProvider.buildBucketBaseUrl(config);
    if (!base) return null;
    return `${base}/${encodeURI(key)}`;
  },
};
