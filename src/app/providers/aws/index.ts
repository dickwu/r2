import { invoke } from '@tauri-apps/api/core';
import type {
  AwsStorageConfig,
  BatchDeleteResult,
  BatchMoveResult,
  ListObjectsOptions,
  ListObjectsResult,
  StorageBucket,
  StorageObject,
  SyncResult,
  MoveOperation,
  StorageProviderAdapter,
} from '@/app/providers/types';

function requireAwsFields(config: AwsStorageConfig) {
  if (!config.accessKeyId || !config.secretAccessKey || !config.region) {
    throw new Error('Missing AWS credentials or region');
  }
}

function toAwsConfigInput(config: AwsStorageConfig) {
  requireAwsFields(config);
  return {
    account_id: config.accountId,
    bucket: config.bucket,
    access_key_id: config.accessKeyId,
    secret_access_key: config.secretAccessKey,
    region: config.region,
    endpoint_scheme: config.endpointScheme ?? null,
    endpoint_host: config.endpointHost ?? null,
    force_path_style: config.forcePathStyle,
  };
}

export const awsProvider: StorageProviderAdapter<AwsStorageConfig> = {
  listBuckets: async (config) => {
    requireAwsFields(config);
    return invoke<StorageBucket[]>('list_aws_buckets', {
      accountId: config.accountId,
      accessKeyId: config.accessKeyId,
      secretAccessKey: config.secretAccessKey,
      region: config.region,
      endpointScheme: config.endpointScheme ?? null,
      endpointHost: config.endpointHost ?? null,
      forcePathStyle: config.forcePathStyle,
    });
  },

  listObjects: async (config, options: ListObjectsOptions = {}) => {
    const { prefix, delimiter, cursor, perPage } = options;
    return invoke<ListObjectsResult>('list_aws_objects', {
      input: {
        config: toAwsConfigInput(config),
        prefix: prefix || null,
        delimiter: delimiter || null,
        continuation_token: cursor || null,
        max_keys: perPage || null,
      },
    });
  },

  listAllObjectsRecursive: async (config) => {
    return invoke<StorageObject[]>('list_all_aws_objects', {
      config: toAwsConfigInput(config),
    });
  },

  listFolderObjects: async (config, prefix) => {
    return invoke<ListObjectsResult>('list_folder_aws_objects', {
      config: toAwsConfigInput(config),
      prefix: prefix || null,
    });
  },

  deleteObject: async (config, key) => {
    return invoke('delete_aws_object', {
      config: toAwsConfigInput(config),
      key,
    });
  },

  batchDeleteObjects: async (config, keys) => {
    return invoke<BatchDeleteResult>('batch_delete_aws_objects', {
      config: toAwsConfigInput(config),
      keys,
    });
  },

  renameObject: async (config, oldKey, newKey) => {
    return invoke('rename_aws_object', {
      config: toAwsConfigInput(config),
      oldKey,
      newKey,
    });
  },

  batchMoveObjects: async (config, operations: MoveOperation[]) => {
    return invoke<BatchMoveResult>('batch_move_aws_objects', {
      config: toAwsConfigInput(config),
      operations,
    });
  },

  generateSignedUrl: async (config, key, expiresIn) => {
    return invoke<string>('generate_aws_signed_url', {
      config: toAwsConfigInput(config),
      key,
      expiresIn,
    });
  },

  uploadContent: async (config, key, content, contentType) => {
    return invoke<string>('upload_aws_content', {
      config: toAwsConfigInput(config),
      key,
      content,
      contentType,
    });
  },

  syncBucket: async (config) => {
    return invoke<SyncResult>('sync_aws_bucket', {
      config: toAwsConfigInput(config),
    });
  },

  buildBucketBaseUrl: (config) => {
    if (config.publicDomain) {
      const scheme = config.publicDomainScheme || 'https';
      return `${scheme}://${config.publicDomain.replace(/\/+$/, '')}`;
    }
    if (!config.bucket || !config.region) return null;
    const scheme = config.endpointScheme || 'https';
    const host = config.endpointHost || `s3.${config.region}.amazonaws.com`;
    if (config.forcePathStyle) {
      return `${scheme}://${host}/${config.bucket}`;
    }
    return `${scheme}://${config.bucket}.${host}`;
  },

  buildPublicUrl: (config, key) => {
    const base = awsProvider.buildBucketBaseUrl(config);
    if (!base) return null;
    return `${base}/${encodeURI(key)}`;
  },
};
