import { describe, expect, test } from 'bun:test';
import { redactSecrets } from './redact';

describe('redactSecrets', () => {
  test('replaces AWS-style access key ids', () => {
    expect(redactSecrets('signing with AKIAIOSFODNN7EXAMPLE failed')).toBe(
      'signing with [redacted-key-id] failed'
    );
  });

  test('replaces authorization header values', () => {
    expect(redactSecrets('Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload.sig')).toBe(
      'Authorization: Bearer [redacted]'
    );
  });

  test('replaces presigned-url credential and signature parameters', () => {
    const url =
      'https://x.example/o?X-Amz-Credential=abc%2F20260902%2Fauto%2Fs3&X-Amz-Signature=deadbeef&X-Amz-Expires=3600';
    expect(redactSecrets(url)).toBe(
      'https://x.example/o?X-Amz-Credential=[redacted]&X-Amz-Signature=[redacted]&X-Amz-Expires=3600'
    );
  });

  test('replaces long hex runs such as R2 keys and account ids', () => {
    const accountId = '0123456789abcdef0123456789abcdef';
    expect(redactSecrets(`PUT https://${accountId}.r2.cloudflarestorage.com/photos/a.png`)).toBe(
      'PUT https://[redacted-hex].r2.cloudflarestorage.com/photos/a.png'
    );
    expect(redactSecrets(`secret ${accountId}${accountId}`)).toBe('secret [redacted-hex]');
  });

  test('replaces scalar values of secret-shaped keys in json and key=value form', () => {
    const config =
      '{"endpoint":"https://s3.example","secret_access_key":"wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY","token":"cf-abc","name":"prod"}';
    expect(redactSecrets(config)).toBe(
      '{"endpoint":"https://s3.example","secret_access_key":"[redacted]","token":"[redacted]","name":"prod"}'
    );
    expect(redactSecrets('login failed for minio password=hunter2 user=admin')).toBe(
      'login failed for minio password=[redacted] user=admin'
    );
    expect(redactSecrets('secretAccessKey: abc123 accessKeyId: AK1')).toBe(
      'secretAccessKey: [redacted] accessKeyId: AK1'
    );
  });

  test('covers compound key names such as api_token, client_secret and accessToken', () => {
    const config =
      '{"api_token":"cf-abc","client_secret":"s3cr3t","accessToken":"at-1","private_key":"pk-1","refresh_token":"rt-1"}';
    expect(redactSecrets(config)).toBe(
      '{"api_token":"[redacted]","client_secret":"[redacted]","accessToken":"[redacted]","private_key":"[redacted]","refresh_token":"[redacted]"}'
    );
  });

  test('leaves nested objects under secret-shaped keys and token-ish identifiers alone', () => {
    const msg =
      'token {"token":{"id":3,"name":"Backup"},"tokenId":3,"objectKey":"photos/a.png","folderKey":"clients/x/"}';
    expect(redactSecrets(msg)).toBe(msg);
  });

  test('replaces bare 40-character tokens but never a path of object keys', () => {
    const cfToken = 'v1Ab3dEf9hIjKlMnOpQrStUvWxYz0123456789_-';
    expect(cfToken).toHaveLength(40);
    expect(redactSecrets(`cloudflare rejected ${cfToken} (403)`)).toBe(
      'cloudflare rejected [redacted-token] (403)'
    );
    const path =
      'Failed to load clients/acme-corp/2026/reports/quarterly-summary-final-draft-v2.pdf';
    expect(redactSecrets(path)).toBe(path);
  });

  test('leaves ordinary messages and short hex alone', () => {
    const msg = 'Failed to list bucket photos: NoSuchBucket (etag 1a2b3c4d)';
    expect(redactSecrets(msg)).toBe(msg);
  });
});
