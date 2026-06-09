import { describe, expect, test, mock } from 'bun:test';
import type {
  AwsStorageConfig,
  R2StorageConfig,
  StorageConfig,
} from '@/app/providers/types';

// Provider adapters import the Tauri core at module load; stub it so importing
// r2cache works in a plain test environment. None of the functions under test
// actually invoke the backend.
mock.module('@tauri-apps/api/core', () => ({
  invoke: async () => undefined,
}));

const { isBucketPublic, buildPublicUrl, buildBucketBaseUrl } = await import('./r2cache');

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
