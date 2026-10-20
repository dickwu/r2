import { describe, expect, mock, test } from 'bun:test';

const tauriCore = await import('@tauri-apps/api/core');
const invokeCalls: Array<{ command: string; args: unknown }> = [];
const detected = {
  mime: 'image/png',
  extension: 'png',
  name: 'Portable Network Graphics',
  contentType: 'application/octet-stream',
};
mock.module('@tauri-apps/api/core', () => ({
  ...tauriCore,
  invoke: async (command: string, args: unknown) => {
    invokeCalls.push({ command, args });
    return detected;
  },
}));

const { detectObjectType } = await import('./fetchPreviewBytes');

describe('detectObjectType', () => {
  test('asks the backend to sniff the object behind the preview URL', async () => {
    const url = 'http://127.0.0.1:9000/bkt/6c4c4cb6?X-Amz-Signature=abc';
    expect(await detectObjectType(url)).toEqual(detected);
    expect(invokeCalls).toEqual([{ command: 'detect_url_type', args: { url } }]);
  });
});
