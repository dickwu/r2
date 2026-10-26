import { describe, expect, test } from 'bun:test';
import { bytesToBase64, contentArgs } from './contentBody';

describe('contentArgs', () => {
  test('text goes as content, bytes as base64', () => {
    expect(contentArgs('hello')).toEqual({ content: 'hello' });
    expect(contentArgs(new Uint8Array([0xff, 0xfe, 0x00]))).toEqual({ contentBase64: '//4A' });
  });

  test('ifMatch is passed only when set', () => {
    expect(contentArgs('x', { ifMatch: '"e1"' })).toEqual({ content: 'x', ifMatch: '"e1"' });
    expect(contentArgs('x', {})).toEqual({ content: 'x' });
  });

  test('large bodies encode without overflowing the stack', () => {
    const big = new Uint8Array(300_000).fill(65);
    const encoded = bytesToBase64(big);
    expect(encoded.length).toBe(400_000);
    expect(atob(encoded).length).toBe(300_000);
  });
});
