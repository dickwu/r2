import { invoke } from '@tauri-apps/api/core';
import type {
  BatchDeleteResult,
  BatchMoveResult,
  ListObjectsOptions,
  ListObjectsResult,
  R2StorageConfig,
  StorageBucket,
  StorageObject,
  SyncResult,
  MoveOperation,
  StorageProviderAdapter,
} from '../types';

function requireR2Credentials(config: R2StorageConfig) {
  if (!config.accessKeyId || !config.secretAccessKey) {
    throw new Error('Missing R2 access key or secret key');
  }
}

function toR2ConfigInput(config: R2StorageConfig) {
  requireR2Credentials(config);
  return {
    account_id: config.accountId,
    bucket: config.bucket,
    access_key_id: config.accessKeyId || '',
    secret_access_key: config.secretAccessKey || '',
  };
}

export const r2Provider: StorageProviderAdapter<R2StorageConfig> = {
  listBuckets: async (config) => {
    requireR2Credentials(config);
    return invoke<StorageBucket[]>('list_r2_buckets', {
      accountId: config.accountId,
      accessKeyId: config.accessKeyId,
      secretAccessKey: config.secretAccessKey,
    });
  },

  listObjects: async (config, options: ListObjectsOptions = {}) => {
    const { prefix, delimiter, cursor, perPage } = options;
    return invoke<ListObjectsResult>('list_r2_objects', {
      input: {
        config: toR2ConfigInput(config),
        prefix: prefix || null,
        delimiter: delimiter || null,
        continuation_token: cursor || null,
        max_keys: perPage || null,
      },
    });
  },

  listAllObjectsRecursive: async (config) => {
    return invoke<StorageObject[]>('list_all_r2_objects', {
      config: toR2ConfigInput(config),
    });
  },

  listFolderObjects: async (config, prefix) => {
    return invoke<ListObjectsResult>('list_folder_r2_objects', {
      config: toR2ConfigInput(config),
      prefix: prefix || null,
    });
  },

  deleteObject: async (config, key) => {
    return invoke('delete_r2_object', {
      config: toR2ConfigInput(config),
      key,
    });
  },

  batchDeleteObjects: async (config, keys) => {
    return invoke<BatchDeleteResult>('batch_delete_r2_objects', {
      config: toR2ConfigInput(config),
      keys,
    });
  },

  renameObject: async (config, oldKey, newKey) => {
    return invoke('rename_r2_object', {
      config: toR2ConfigInput(config),
      oldKey,
      newKey,
    });
  },

  batchMoveObjects: async (config, operations: MoveOperation[]) => {
    return invoke<BatchMoveResult>('batch_move_r2_objects', {
      config: toR2ConfigInput(config),
      operations,
    });
  },

  generateSignedUrl: async (config, key, expiresIn) => {
    return invoke<string>('generate_signed_url', {
      config: toR2ConfigInput(config),
      key,
      expiresIn,
    });
  },

  uploadContent: async (config, key, content, contentType) => {
    return invoke<string>('upload_r2_content', {
      config: toR2ConfigInput(config),
      key,
      content,
      contentType,
    });
  },

  syncBucket: async (config) => {
    return invoke<SyncResult>('sync_bucket', {
      config: toR2ConfigInput(config),
    });
  },

  buildBucketBaseUrl: (config) => {
    if (config.publicDomain) {
      const scheme = config.publicDomainScheme || 'https';
      return `${scheme}://${config.publicDomain.replace(/\/+$/, '')}`;
    }
    if (!config.accountId || !config.bucket) return null;
    return `https://${config.accountId}.r2.cloudflarestorage.com/${config.bucket}`;
  },

  buildPublicUrl: (config, key) => {
    const base = r2Provider.buildBucketBaseUrl(config);
    if (!base) return null;
    return `${base}/${encodeURI(key)}`;
  },
};
