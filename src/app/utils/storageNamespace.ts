import type { StorageConfig } from '@/app/lib/r2cache';

function endpoint(scheme: string, host: string): string {
  try {
    return new URL(`${scheme.trim()}://${host.trim()}`).toString().replace(/\/+$/, '');
  } catch {
    // Invalid input still gets an isolated key; the backend reports validation.
    return `${scheme}://${host}`;
  }
}

/** Internal hash input. Never use this credential-bearing value as a query key or log it. */
export function storageNamespaceInput(config: StorageConfig | null): string | null {
  if (!config) return null;
  let host = '';
  let region = 'us-east-1';
  let pathStyle = true;
  if (config.provider === 'r2') {
    host = `https://${config.accountId}.r2.cloudflarestorage.com`;
    region = 'auto';
  } else if (config.provider === 'aws') {
    host = config.endpointHost?.trim()
      ? endpoint(config.endpointScheme ?? 'https', config.endpointHost)
      : '';
    region = config.region;
    pathStyle = config.forcePathStyle;
  } else {
    host = endpoint(config.endpointScheme, config.endpointHost);
    pathStyle = config.forcePathStyle;
  }
  return JSON.stringify([
    'cache-scope-v1',
    config.provider,
    config.accountId,
    host,
    region,
    pathStyle,
    config.accessKeyId ?? '',
    config.secretAccessKey ?? '',
  ]);
}

export async function hashStorageNamespace(input: string): Promise<string> {
  const bytes = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(input));
  return Array.from(new Uint8Array(bytes), (byte) => byte.toString(16).padStart(2, '0')).join('');
}

export async function storageNamespace(config: StorageConfig): Promise<string> {
  return hashStorageNamespace(storageNamespaceInput(config)!);
}
