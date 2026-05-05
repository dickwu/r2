import { describe, expect, test } from 'bun:test';
import { awsProvider } from '@/app/providers/aws';
import { minioProvider } from '@/app/providers/minio';
import { r2Provider } from '@/app/providers/r2';
import { rustfsProvider } from '@/app/providers/rustfs';
import type {
  AwsStorageConfig,
  MinioStorageConfig,
  R2StorageConfig,
  RustfsStorageConfig,
} from '@/app/providers/types';
import { encodeObjectKeyForUrl } from './objectKeyUrl';

const encodingCases: Array<[string, string]> = [
  ['plain.txt', 'plain.txt'],
  ['a b.txt', 'a%20b.txt'],
  ['a#b.txt', 'a%23b.txt'],
  ['a?b.txt', 'a%3Fb.txt'],
  ['100%.txt', '100%25.txt'],
  ['a+b.txt', 'a%2Bb.txt'],
  ['folder/hello world.txt', 'folder/hello%20world.txt'],
  ['folder/你好.png', 'folder/%E4%BD%A0%E5%A5%BD.png'],
  ['folder/nested/file#1?.txt', 'folder/nested/file%231%3F.txt'],
];

describe('encodeObjectKeyForUrl', () => {
  for (const [key, expected] of encodingCases) {
    test(`encodes ${key} as ${expected}`, () => {
      expect(encodeObjectKeyForUrl(key)).toBe(expected);
    });
  }
});

describe('provider public URL object-key encoding', () => {
  const publicUrlCases = [
    {
      name: 'R2',
      buildUrl: () =>
        r2Provider.buildPublicUrl(
          {
            provider: 'r2',
            accountId: 'acct',
            bucket: 'bucket',
            accessKeyId: 'access',
            secretAccessKey: 'secret',
          } satisfies R2StorageConfig,
          'folder/nested/file#1?.txt'
        ),
      expected: 'https://acct.r2.cloudflarestorage.com/bucket/folder/nested/file%231%3F.txt',
    },
    {
      name: 'AWS virtual-hosted',
      buildUrl: () =>
        awsProvider.buildPublicUrl(
          {
            provider: 'aws',
            accountId: 'acct',
            bucket: 'bucket',
            accessKeyId: 'access',
            secretAccessKey: 'secret',
            region: 'us-east-1',
            forcePathStyle: false,
          } satisfies AwsStorageConfig,
          'folder/nested/file#1?.txt'
        ),
      expected: 'https://bucket.s3.us-east-1.amazonaws.com/folder/nested/file%231%3F.txt',
    },
    {
      name: 'MinIO path-style',
      buildUrl: () =>
        minioProvider.buildPublicUrl(
          {
            provider: 'minio',
            accountId: 'acct',
            bucket: 'bucket',
            accessKeyId: 'access',
            secretAccessKey: 'secret',
            endpointScheme: 'http',
            endpointHost: 'storage.local:9000',
            forcePathStyle: true,
          } satisfies MinioStorageConfig,
          'folder/nested/file#1?.txt'
        ),
      expected: 'http://storage.local:9000/bucket/folder/nested/file%231%3F.txt',
    },
    {
      name: 'RustFS',
      buildUrl: () =>
        rustfsProvider.buildPublicUrl(
          {
            provider: 'rustfs',
            accountId: 'acct',
            bucket: 'bucket',
            accessKeyId: 'access',
            secretAccessKey: 'secret',
            endpointScheme: 'http',
            endpointHost: 'rustfs.local:9000',
            forcePathStyle: true,
          } satisfies RustfsStorageConfig,
          'folder/nested/file#1?.txt'
        ),
      expected: 'http://rustfs.local:9000/bucket/folder/nested/file%231%3F.txt',
    },
  ];

  for (const publicUrlCase of publicUrlCases) {
    test(`${publicUrlCase.name} encodes reserved characters in public URLs`, () => {
      expect(publicUrlCase.buildUrl()).toBe(publicUrlCase.expected);
    });
  }
});
