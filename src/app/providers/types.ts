export type StorageProvider = 'r2' | 'aws' | 'minio' | 'rustfs';

export interface StorageBucket {
  name: string;
  creation_date: string;
}

export interface StorageObject {
  key: string;
  size: number;
  last_modified: string;
  etag: string;
}

export interface ListObjectsResult {
  objects: StorageObject[];
  folders: string[];
  truncated: boolean;
  continuation_token?: string;
}

export interface ListObjectsOptions {
  prefix?: string;
  delimiter?: string;
  cursor?: string;
  perPage?: number;
}

export interface BaseStorageConfig {
  provider: StorageProvider;
  accountId: string;
  bucket: string;
  publicDomain?: string;
  publicDomainScheme?: string;
}

export interface R2StorageConfig extends BaseStorageConfig {
  provider: 'r2';
  token?: string;
  accessKeyId?: string;
  secretAccessKey?: string;
}

export interface AwsStorageConfig extends BaseStorageConfig {
  provider: 'aws';
  accessKeyId: string;
  secretAccessKey: string;
  region: string;
  endpointScheme?: string;
  endpointHost?: string;
  forcePathStyle: boolean;
}

export interface MinioStorageConfig extends BaseStorageConfig {
  provider: 'minio';
  accessKeyId: string;
  secretAccessKey: string;
  endpointScheme: string;
  endpointHost: string;
  forcePathStyle: boolean;
}

export interface RustfsStorageConfig extends BaseStorageConfig {
  provider: 'rustfs';
  accessKeyId: string;
  secretAccessKey: string;
  endpointScheme: string;
  endpointHost: string;
  forcePathStyle: boolean;
}

export type StorageConfig =
  | R2StorageConfig
  | AwsStorageConfig
  | MinioStorageConfig
  | RustfsStorageConfig;

export interface SyncResult {
  count: number;
  timestamp: number;
}

export interface MoveOperation {
  old_key: string;
  new_key: string;
}

export interface UploadFileInput {
  taskId: string;
  filePath: string;
  key: string;
  contentType?: string;
}

export interface UploadFileResult {
  task_id: string;
  success: boolean;
  error?: string;
  upload_id?: string;
}

export interface BatchDeleteResult {
  deleted: number;
  failed: number;
  errors: string[];
}

export interface BatchMoveResult {
  moved: number;
  failed: number;
  errors: string[];
}

export interface StorageProviderAdapter<TConfig extends StorageConfig = StorageConfig> {
  listBuckets: (config: TConfig) => Promise<StorageBucket[]>;
  listObjects: (config: TConfig, options?: ListObjectsOptions) => Promise<ListObjectsResult>;
  listAllObjectsRecursive: (config: TConfig) => Promise<StorageObject[]>;
  listFolderObjects: (config: TConfig, prefix?: string) => Promise<ListObjectsResult>;
  deleteObject: (config: TConfig, key: string) => Promise<void>;
  batchDeleteObjects: (config: TConfig, keys: string[]) => Promise<BatchDeleteResult>;
  renameObject: (config: TConfig, oldKey: string, newKey: string) => Promise<void>;
  batchMoveObjects: (config: TConfig, operations: MoveOperation[]) => Promise<BatchMoveResult>;
  generateSignedUrl: (config: TConfig, key: string, expiresIn?: number) => Promise<string>;
  uploadFile: (config: TConfig, input: UploadFileInput) => Promise<UploadFileResult>;
  uploadContent: (
    config: TConfig,
    key: string,
    content: string,
    contentType?: string
  ) => Promise<string>;
  syncBucket: (config: TConfig) => Promise<SyncResult>;
  buildBucketBaseUrl: (config: TConfig) => string | null;
  buildPublicUrl: (config: TConfig, key: string) => string | null;
}
