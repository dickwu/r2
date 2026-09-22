import { describe, expect, test, mock } from 'bun:test';
import type {
  AwsStorageConfig,
  MinioStorageConfig,
  R2StorageConfig,
  StorageConfig,
} from '@/app/providers/types';

// Preserve exports used by Tauri's event module regardless of test order.
// None of the functions under test actually invoke the backend.
const tauriCore = await import('@tauri-apps/api/core');
const invokeCalls: Array<{ command: string; args: any }> = [];
let invokeMock: (command: string, args: any) => Promise<unknown> = async () => undefined;
mock.module('@tauri-apps/api/core', () => ({
  ...tauriCore,
  invoke: async (command: string, args: any) => {
    invokeCalls.push({ command, args });
    return invokeMock(command, args);
  },
}));

const {
  isBucketPublic,
  buildPublicUrl,
  buildBucketBaseUrl,
  hasSigningCredentials,
  getPrefixCache,
} = await import('./r2cache');

function r2(overrides: Partial<R2StorageConfig> = {}): R2StorageConfig {
  return {
    provider: 'r2',
    accountId: 'acct',
    bucket: 'bkt',
    ...overrides,
  };
}

function aws(overrides: Partial<AwsStorageConfig> = {}): AwsStorageConfig {
  return {
    provider: 'aws',
    accountId: 'aws',
    bucket: 'bkt',
    accessKeyId: 'ak',
    secretAccessKey: 'sk',
    region: 'us-east-1',
    forcePathStyle: false,
    ...overrides,
  };
}

function minio(overrides: Partial<MinioStorageConfig> = {}): MinioStorageConfig {
  return {
    provider: 'minio',
    accountId: 'minio',
    bucket: 'bkt',
    accessKeyId: 'ak',
    secretAccessKey: 'sk',
    endpointScheme: 'https',
    endpointHost: 'minio.example.com',
    forcePathStyle: true,
    ...overrides,
  };
}

describe('hasSigningCredentials', () => {
  test('requires both S3 keys', () => {
    expect(hasSigningCredentials(r2())).toBe(false);
    expect(hasSigningCredentials(r2({ accessKeyId: 'ak' }))).toBe(false);
    expect(hasSigningCredentials(r2({ accessKeyId: 'ak', secretAccessKey: 'sk' }))).toBe(true);
  });

  test('an R2 API token alone cannot sign S3 URLs', () => {
    expect(hasSigningCredentials(r2({ token: 'api-token' }))).toBe(false);
  });

  test('AWS additionally needs a region', () => {
    expect(hasSigningCredentials(aws({ region: '' }))).toBe(false);
    expect(hasSigningCredentials(aws())).toBe(true);
  });

  test('MinIO-style providers additionally need an endpoint', () => {
    expect(hasSigningCredentials(minio())).toBe(true);
    expect(hasSigningCredentials(minio({ endpointHost: '' }))).toBe(false);
    expect(hasSigningCredentials(minio({ endpointScheme: '' }))).toBe(false);
  });

  test('null/undefined config cannot sign', () => {
    expect(hasSigningCredentials(null)).toBe(false);
    expect(hasSigningCredentials(undefined)).toBe(false);
  });
});

describe('isBucketPublic', () => {
  test('returns false when isPublic is unset', () => {
    expect(isBucketPublic(r2({ publicDomain: 'cdn.example.com' }))).toBe(false);
  });

  test('R2 requires a public domain even when isPublic is true', () => {
    expect(isBucketPublic(r2({ isPublic: true }))).toBe(false);
    expect(isBucketPublic(r2({ isPublic: true, publicDomain: 'cdn.example.com' }))).toBe(true);
  });

  test('S3-family providers are public from their endpoint without a domain', () => {
    expect(isBucketPublic(aws({ isPublic: true }))).toBe(true);
    expect(isBucketPublic(aws({ isPublic: false }))).toBe(false);
  });

  test('null/undefined config is never public', () => {
    expect(isBucketPublic(null)).toBe(false);
    expect(isBucketPublic(undefined)).toBe(false);
  });
});

describe('R2 buildPublicUrl honors the public path prefix', () => {
  test('without a prefix the key sits at the domain root', () => {
    const cfg = r2({ isPublic: true, publicDomain: 'cdn.example.com' }) as StorageConfig;
    expect(buildPublicUrl(cfg, 'images/a.jpg')).toBe('https://cdn.example.com/images/a.jpg');
  });

  test('a prefix is inserted between the domain and the key', () => {
    const cfg = r2({
      isPublic: true,
      publicDomain: 'cdn.example.com',
      publicPathPrefix: 'assets',
    }) as StorageConfig;
    expect(buildBucketBaseUrl(cfg)).toBe('https://cdn.example.com/assets');
    expect(buildPublicUrl(cfg, 'images/a.jpg')).toBe('https://cdn.example.com/assets/images/a.jpg');
  });

  test('surrounding slashes on the prefix are normalized', () => {
    const cfg = r2({
      isPublic: true,
      publicDomain: 'cdn.example.com',
      publicPathPrefix: '/assets/',
    }) as StorageConfig;
    expect(buildPublicUrl(cfg, 'a.jpg')).toBe('https://cdn.example.com/assets/a.jpg');
  });
});

describe('S3-family providers support domain + prefix too', () => {
  test('AWS public bucket with a custom domain + prefix', () => {
    const cfg = aws({
      isPublic: true,
      publicDomain: 'cdn.aws.example.com',
      publicPathPrefix: 'media',
    }) as StorageConfig;
    expect(buildPublicUrl(cfg, 'x/y.png')).toBe('https://cdn.aws.example.com/media/x/y.png');
  });

  test('AWS public bucket without a domain derives from the endpoint (vhost style)', () => {
    const cfg = aws({ isPublic: true }) as StorageConfig;
    expect(buildPublicUrl(cfg, 'x.png')).toBe('https://bkt.s3.us-east-1.amazonaws.com/x.png');
  });
});

describe('paged prefix cache IPC', () => {
  test('streams cache pages without invoking the aggregate cache command', async () => {
    invokeCalls.length = 0;
    const cfg = r2({ accessKeyId: 'ak', secretAccessKey: 'sk' }) as StorageConfig;
    invokeMock = async (_command, args) => {
      const input = args.input;
      const base = {
        provider: input.provider,
        account_id: input.account_id,
        bucket: input.bucket,
        prefix: input.prefix,
        request_id: input.request_id,
        generation: input.generation,
        from_cache: true,
        freshness: 'partial',
      };
      if (!input.cache_cursor) {
        return {
          ...base,
          files: Array.from({ length: 1000 }, (_, index) => ({
            key: `file-${index.toString().padStart(4, '0')}.txt`,
            name: `file-${index.toString().padStart(4, '0')}.txt`,
            size: index,
            last_modified: 'old',
          })),
          folders: [],
          page_index: 0,
          next_cursor: 'cursor-1',
          complete: false,
        };
      }
      return {
        ...base,
        files: [
          {
            key: 'file-1000.txt',
            name: 'file-1000.txt',
            size: 1000,
            last_modified: 'old',
          },
        ],
        folders: [],
        page_index: 1,
        next_cursor: null,
        complete: true,
        freshness: 'stale',
      };
    };
    const updates: number[] = [];
    const snapshot = await getPrefixCache(cfg, '', {
      onUpdate: (update) => updates.push(update.items.length),
    });
    expect(snapshot?.items).toHaveLength(1001);
    expect(snapshot?.freshness).toBe('stale');
    expect(updates).toEqual([1000, 1001]);
    expect(invokeCalls.map((call) => call.command)).toEqual([
      'get_prefix_cache_page',
      'get_prefix_cache_page',
    ]);
  });
});
