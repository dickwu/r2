import { fetch } from '@tauri-apps/plugin-http';
import type { R2Config } from '../components/ConfigModal';

export interface R2Object {
  key: string;
  size: number;
  last_modified: string;
  etag: string;
  http_metadata?: {
    contentType?: string;
  };
  storage_class?: string;
}

export interface ListObjectsResult {
  objects: R2Object[];
  folders: string[];
  truncated: boolean;
  cursor?: string;
}

interface CloudflareR2Response {
  success: boolean;
  errors: Array<{ code: number; message: string }>;
  messages: string[];
  result_info?: {
    cursor?: string;
    is_truncated?: boolean;
    per_page?: number;
    delimited?: string[];
  };
  result: R2Object[];
}

export interface ListObjectsOptions {
  prefix?: string;
  delimiter?: string;
  cursor?: string;
  perPage?: number;
}

export interface R2Bucket {
  name: string;
  creation_date: string;
}

interface ListBucketsResponse {
  success: boolean;
  errors: Array<{ code: number; message: string }>;
  result: { buckets: R2Bucket[] };
}

// List all buckets in account
export async function listR2Buckets(accountId: string, token: string): Promise<R2Bucket[]> {
  const url = `https://api.cloudflare.com/client/v4/accounts/${accountId}/r2/buckets`;

  const response = await fetch(url, {
    method: 'GET',
    headers: {
      Authorization: `Bearer ${token}`,
      'Content-Type': 'application/json',
    },
  });

  if (!response.ok) {
    const text = await response.text();
    throw new Error(`R2 API error: ${response.status} - ${text}`);
  }

  const data: ListBucketsResponse = await response.json();

  if (!data.success) {
    throw new Error(data.errors.map((e) => e.message).join(', '));
  }

  return data.result.buckets ?? [];
}

export async function listR2Objects(
  config: R2Config,
  options: ListObjectsOptions = {}
): Promise<ListObjectsResult> {
  const { prefix = '', delimiter = '/', cursor, perPage = 1000 } = options;
  const url = `https://api.cloudflare.com/client/v4/accounts/${config.accountId}/r2/buckets/${config.bucket}/objects`;

  const params = new URLSearchParams();
  if (prefix) params.set('prefix', prefix);
  if (delimiter) params.set('delimiter', delimiter);
  if (cursor) params.set('cursor', cursor);
  params.set('per_page', perPage.toString());

  const response = await fetch(`${url}?${params}`, {
    method: 'GET',
    headers: {
      Authorization: `Bearer ${config.token}`,
      'Content-Type': 'application/json',
    },
  });

  if (!response.ok) {
    const text = await response.text();
    throw new Error(`R2 API error: ${response.status} - ${text}`);
  }

  const data: CloudflareR2Response = await response.json();

  if (!data.success) {
    throw new Error(data.errors.map((e) => e.message).join(', '));
  }

  return {
    objects: data.result ?? [],
    folders: data.result_info?.delimited ?? [],
    truncated: data.result_info?.is_truncated ?? false,
    cursor: data.result_info?.cursor,
  };
}

// Load all objects at a path (handles pagination)
export async function listAllR2Objects(
  config: R2Config,
  prefix: string = ''
): Promise<ListObjectsResult> {
  const allObjects: R2Object[] = [];
  const allFolders: string[] = [];
  let cursor: string | undefined;

  do {
    const result = await listR2Objects(config, { prefix, cursor, delimiter: '/' });
    allObjects.push(...result.objects);

    // Only add unique folders
    for (const folder of result.folders) {
      if (!allFolders.includes(folder)) {
        allFolders.push(folder);
      }
    }

    cursor = result.truncated ? result.cursor : undefined;
  } while (cursor);

  return {
    objects: allObjects,
    folders: allFolders,
    truncated: false,
  };
}

// Fetch ALL files in bucket recursively (for IndexedDB caching)
export async function listAllR2ObjectsRecursive(config: R2Config): Promise<R2Object[]> {
  const allObjects: R2Object[] = [];
  let cursor: string | undefined;

  do {
    const result = await listR2Objects(config, { cursor, delimiter: '' });
    allObjects.push(...result.objects);
    cursor = result.truncated ? result.cursor : undefined;
  } while (cursor);

  return allObjects;
}

// Delete a single object from R2
export async function deleteR2Object(config: R2Config, key: string): Promise<void> {
  const url = `https://api.cloudflare.com/client/v4/accounts/${config.accountId}/r2/buckets/${config.bucket}/objects/${encodeURIComponent(key)}`;

  const response = await fetch(url, {
    method: 'DELETE',
    headers: {
      Authorization: `Bearer ${config.token}`,
    },
  });

  if (!response.ok) {
    const text = await response.text();
    throw new Error(`Failed to delete: ${response.status} - ${text}`);
  }
}

// Generate a presigned URL for S3-compatible R2 access
export async function generateSignedUrl(
  accountId: string,
  bucket: string,
  key: string,
  accessKeyId: string,
  secretAccessKey: string,
  expiresIn: number = 3600 // 1 hour default
): Promise<string> {
  const { S3Client, GetObjectCommand } = await import('@aws-sdk/client-s3');
  const { getSignedUrl } = await import('@aws-sdk/s3-request-presigner');

  const client = new S3Client({
    region: 'auto',
    endpoint: `https://${accountId}.r2.cloudflarestorage.com`,
    // forcePathStyle: true,
    credentials: {
      accessKeyId,
      secretAccessKey,
    },
  });

  const command = new GetObjectCommand({ Bucket: bucket, Key: key });
  return getSignedUrl(client, command, { expiresIn });
}

// Verify S3 credentials using HEAD bucket request
export async function verifyS3Credentials(
  accountId: string,
  bucket: string,
  accessKeyId: string,
  secretAccessKey: string
): Promise<void> {
  const { S3Client, HeadBucketCommand } = await import('@aws-sdk/client-s3');

  const client = new S3Client({
    region: 'auto',
    endpoint: `https://${accountId}.r2.cloudflarestorage.com`,
    credentials: {
      accessKeyId,
      secretAccessKey,
    },
  });

  await client.send(new HeadBucketCommand({ Bucket: bucket }));
}
