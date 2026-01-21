import type {
  AwsStorageConfig,
  MinioStorageConfig,
  R2StorageConfig,
  RustfsStorageConfig,
  StorageConfig,
  StorageProviderAdapter,
} from '@/app/providers/types';
import { r2Provider } from '@/app/providers/r2';
import { awsProvider } from '@/app/providers/aws';
import { minioProvider } from '@/app/providers/minio';
import { rustfsProvider } from '@/app/providers/rustfs';

export function getProviderAdapter(
  config: R2StorageConfig
): StorageProviderAdapter<R2StorageConfig>;
export function getProviderAdapter(
  config: AwsStorageConfig
): StorageProviderAdapter<AwsStorageConfig>;
export function getProviderAdapter(
  config: MinioStorageConfig
): StorageProviderAdapter<MinioStorageConfig>;
export function getProviderAdapter(
  config: RustfsStorageConfig
): StorageProviderAdapter<RustfsStorageConfig>;
export function getProviderAdapter(config: StorageConfig): StorageProviderAdapter<StorageConfig>;
export function getProviderAdapter(config: StorageConfig): StorageProviderAdapter {
  switch (config.provider) {
    case 'r2':
      return r2Provider as StorageProviderAdapter<StorageConfig>;
    case 'aws':
      return awsProvider as StorageProviderAdapter<StorageConfig>;
    case 'minio':
      return minioProvider as StorageProviderAdapter<StorageConfig>;
    case 'rustfs':
      return rustfsProvider as StorageProviderAdapter<StorageConfig>;
    default:
      return r2Provider as StorageProviderAdapter<StorageConfig>;
  }
}
