import { describe, expect, mock, test } from 'bun:test';

const httpPlugin = await import('@tauri-apps/plugin-http');
const requests: Array<{ url: string; init?: RequestInit }> = [];
let respond: () => Response = () => new Response(null);
mock.module('@tauri-apps/plugin-http', () => ({
  ...httpPlugin,
  fetch: async (url: string, init?: RequestInit) => {
    requests.push({ url, init });
    return respond();
  },
}));

const { fetchContentType } = await import('./fetchPreviewBytes');

/** A body that records whether it was cancelled instead of read. */
function trackedBody() {
  const body = { cancelled: false, stream: null as unknown as ReadableStream<Uint8Array> };
  body.stream = new ReadableStream<Uint8Array>({
    cancel() {
      body.cancelled = true;
    },
  });
  return body;
}

describe('fetchContentType', () => {
  test('asks for a single byte, since a HEAD fails a GET-presigned URL', async () => {
    const body = trackedBody();
    respond = () =>
      new Response(body.stream, { status: 206, headers: { 'Content-Type': 'image/jpeg' } });

    expect(await fetchContentType('https://bucket.example/6c4c4cb6')).toBe('image/jpeg');
    const { init } = requests[requests.length - 1];
    expect(init?.method ?? 'GET').toBe('GET');
    expect(new Headers(init?.headers).get('range')).toBe('bytes=0-0');
    expect(body.cancelled).toBe(true);
  });

  test('cancels the body when the server ignores the range', async () => {
    const body = trackedBody();
    respond = () =>
      new Response(body.stream, { status: 200, headers: { 'Content-Type': 'video/mp4' } });

    expect(await fetchContentType('https://bucket.example/clip')).toBe('video/mp4');
    expect(body.cancelled).toBe(true);
  });

  test('resolves null when the object has no type', async () => {
    respond = () => new Response(trackedBody().stream, { status: 206 });
    expect(await fetchContentType('https://bucket.example/blob')).toBeNull();
  });

  test('fails on an error status rather than reading the error body type', async () => {
    respond = () =>
      new Response('<Error><Code>AccessDenied</Code></Error>', {
        status: 403,
        statusText: 'Forbidden',
        headers: { 'Content-Type': 'application/xml' },
      });
    const failure = await fetchContentType('https://bucket.example/private').catch(
      (err: Error) => err.message
    );
    expect(failure).toBe('HTTP 403: Forbidden');
  });
});
