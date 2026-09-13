import { describe, expect, test } from 'bun:test';
import { QueryClient } from '@tanstack/react-query';
import type { MinioStorageConfig } from '@/app/providers/types';
import { storageNamespace } from './storageNamespace';

const base: MinioStorageConfig = {
  provider: 'minio',
  accountId: 'same-account',
  bucket: 'bucket',
  accessKeyId: 'key',
  secretAccessKey: 'private-secret',
  endpointScheme: 'https',
  endpointHost: 'a.example',
  forcePathStyle: true,
};

describe('connection namespace for frontend caches', () => {
  test('preserves a normalized same-namespace warm folder query', async () => {
    const first = await storageNamespace(base);
    const normalized = await storageNamespace({ ...base, endpointHost: 'A.EXAMPLE:443/' });
    expect(first).toBe(normalized);
    const client = new QueryClient();
    const key = (scope: string) => [
      'folder-contents',
      base.provider,
      base.accountId,
      base.bucket,
      '',
      scope,
    ];
    client.setQueryData(key(first), { items: ['warm'] });
    expect(client.getQueryData(key(normalized))).toEqual({ items: ['warm'] });
    const edited = await storageNamespace({ ...base, endpointHost: 'b.example' });
    expect(client.getQueryData(key(edited))).toBeUndefined();
    // Even a late A response is isolated in A's key.
    client.setQueryData(key(first), { items: ['late-a'] });
    expect(client.getQueryData(key(edited))).toBeUndefined();
    expect(JSON.stringify(key(first))).not.toContain('private-secret');
    client.clear();
  });

  test('distinguishes authentication, region, and addressing changes', async () => {
    const original = await storageNamespace(base);
    expect(await storageNamespace({ ...base, secretAccessKey: 'rotated' })).not.toBe(original);
    expect(await storageNamespace({ ...base, accessKeyId: 'other' })).not.toBe(original);
    expect(await storageNamespace({ ...base, forcePathStyle: false })).not.toBe(original);
    const aws = { ...base, provider: 'aws' as const, region: 'us-east-1' };
    expect(await storageNamespace(aws)).not.toBe(
      await storageNamespace({ ...aws, region: 'us-west-2' })
    );
  });
});
