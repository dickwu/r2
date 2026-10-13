import { fetch as tauriFetch } from '@tauri-apps/plugin-http';
import { invoke } from '@tauri-apps/api/core';

export interface PreviewDownloadProgress {
  loaded: number;
  total: number | null;
}

export interface FetchPreviewBytesOptions {
  signal?: AbortSignal;
  onProgress?: (progress: PreviewDownloadProgress) => void;
}

export class PreviewFetchAbortedError extends Error {
  constructor() {
    super('Preview download cancelled');
    this.name = 'PreviewFetchAbortedError';
  }
}

function isScopeError(err: unknown): boolean {
  if (!err) return false;
  const message = err instanceof Error ? err.message : typeof err === 'string' ? err : '';
  return message.toLowerCase().includes('url not allowed on the configured scope');
}

function base64ToBytes(base64: string): Uint8Array {
  const binaryString = atob(base64);
  const bytes = new Uint8Array(binaryString.length);
  for (let i = 0; i < binaryString.length; i++) {
    bytes[i] = binaryString.charCodeAt(i);
  }
  return bytes;
}

async function fetchViaBackend(url: string): Promise<Uint8Array> {
  const base64Data = await invoke<string>('fetch_url_bytes', { url });
  return base64ToBytes(base64Data);
}

/**
 * Downloads an object for in-app preview through Tauri's HTTP plugin, which
 * runs the request in Rust: the webview's own fetch would be blocked by CORS
 * on signed R2/S3 URLs. Streams the body so callers can show progress, and
 * falls back to the `fetch_url_bytes` command when the plugin's URL scope
 * rejects the host.
 */
export async function fetchPreviewBytes(
  url: string,
  { signal, onProgress }: FetchPreviewBytesOptions = {}
): Promise<Uint8Array> {
  const throwIfAborted = () => {
    if (signal?.aborted) throw new PreviewFetchAbortedError();
  };

  try {
    const response = await tauriFetch(url, { signal });
    if (!response.ok) {
      throw new Error(`HTTP ${response.status}: ${response.statusText}`);
    }

    const contentLength = response.headers.get('content-length');
    const total = contentLength ? parseInt(contentLength, 10) : null;

    if (!response.body) {
      const bytes = new Uint8Array(await response.arrayBuffer());
      throwIfAborted();
      onProgress?.({ loaded: bytes.length, total: bytes.length });
      return bytes;
    }

    const reader = response.body.getReader();
    const chunks: Uint8Array[] = [];
    let loaded = 0;
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      if (signal?.aborted) {
        await reader.cancel();
        throw new PreviewFetchAbortedError();
      }
      chunks.push(value);
      loaded += value.length;
      onProgress?.({ loaded, total });
    }

    const combined = new Uint8Array(loaded);
    let offset = 0;
    for (const chunk of chunks) {
      combined.set(chunk, offset);
      offset += chunk.length;
    }
    return combined;
  } catch (err) {
    throwIfAborted();
    if (err instanceof PreviewFetchAbortedError || !isScopeError(err)) throw err;
    const bytes = await fetchViaBackend(url);
    throwIfAborted();
    onProgress?.({ loaded: bytes.length, total: bytes.length });
    return bytes;
  }
}
