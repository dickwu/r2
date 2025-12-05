import { fetch } from "@tauri-apps/plugin-http";
import type { R2Config } from "../components/ConfigModal";

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

export async function listR2Objects(
  config: R2Config,
  options: ListObjectsOptions = {}
): Promise<ListObjectsResult> {
  const { prefix = "", delimiter = "/", cursor, perPage = 1000 } = options;
  const url = `https://api.cloudflare.com/client/v4/accounts/${config.accountId}/r2/buckets/${config.bucket}/objects`;

  const params = new URLSearchParams();
  if (prefix) params.set("prefix", prefix);
  if (delimiter) params.set("delimiter", delimiter);
  if (cursor) params.set("cursor", cursor);
  params.set("per_page", perPage.toString());

  const response = await fetch(`${url}?${params}`, {
    method: "GET",
    headers: {
      Authorization: `Bearer ${config.token}`,
      "Content-Type": "application/json",
    },
  });

  if (!response.ok) {
    const text = await response.text();
    throw new Error(`R2 API error: ${response.status} - ${text}`);
  }

  const data: CloudflareR2Response = await response.json();

  if (!data.success) {
    throw new Error(data.errors.map((e) => e.message).join(", "));
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
  prefix: string = ""
): Promise<ListObjectsResult> {
  const allObjects: R2Object[] = [];
  const allFolders: string[] = [];
  let cursor: string | undefined;

  do {
    const result = await listR2Objects(config, { prefix, cursor, delimiter: "/" });
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
export async function listAllR2ObjectsRecursive(
  config: R2Config
): Promise<R2Object[]> {
  const allObjects: R2Object[] = [];
  let cursor: string | undefined;

  do {
    const result = await listR2Objects(config, { cursor, delimiter: "" });
    allObjects.push(...result.objects);
    cursor = result.truncated ? result.cursor : undefined;
  } while (cursor);

  return allObjects;
}
