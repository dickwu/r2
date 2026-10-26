import { describe, expect, test } from 'bun:test';
import {
  applyLineEnding,
  decodeText,
  describeFormat,
  detectLineEnding,
  encodeText,
  isPlainUtf8,
} from './textCodec';

const bytes = (...values: number[]) => new Uint8Array(values);
const utf8 = (s: string) => new TextEncoder().encode(s);

function roundTrip(input: Uint8Array) {
  const decoded = decodeText(input);
  if (!decoded.ok) throw new Error(decoded.reason);
  return { decoded, encoded: encodeText(decoded.text, decoded.format) };
}

describe('line endings', () => {
  test('detects the dominant line ending', () => {
    expect(detectLineEnding('a\r\nb\r\nc\n')).toBe('\r\n');
    expect(detectLineEnding('a\nb\n')).toBe('\n');
    expect(detectLineEnding('a\rb\r')).toBe('\r');
    expect(detectLineEnding('no breaks')).toBe('\n');
  });

  test('applies the file line ending to editor text', () => {
    expect(applyLineEnding('a\nb\n', '\r\n')).toBe('a\r\nb\r\n');
    expect(applyLineEnding('a\r\nb', '\n')).toBe('a\nb');
  });
});

describe('decodeText / encodeText', () => {
  test('plain UTF-8 round-trips byte for byte', () => {
    const input = utf8('héllo\nwörld 你好\n');
    const { decoded, encoded } = roundTrip(input);
    expect(decoded.ok && isPlainUtf8(decoded.format)).toBe(true);
    expect(Array.from(encoded)).toEqual(Array.from(input));
  });

  test('CRLF files are edited with \\n and saved with CRLF', () => {
    const input = utf8('a\r\nb\r\n');
    const { decoded, encoded } = roundTrip(input);
    expect(decoded.ok && decoded.text).toBe('a\nb\n');
    expect(Array.from(encoded)).toEqual(Array.from(input));
  });

  test('a UTF-8 BOM is kept', () => {
    const input = bytes(0xef, 0xbb, 0xbf, ...utf8('x,y\n'));
    const { decoded, encoded } = roundTrip(input);
    expect(decoded.ok && decoded.text).toBe('x,y\n');
    expect(decoded.ok && decoded.format.bom).toBe(true);
    expect(decoded.ok && isPlainUtf8(decoded.format)).toBe(false);
    expect(Array.from(encoded)).toEqual(Array.from(input));
  });

  test('UTF-16 with a BOM round-trips in both byte orders', () => {
    const le = bytes(0xff, 0xfe, 0x68, 0x00, 0x69, 0x00, 0x0a, 0x00);
    const be = bytes(0xfe, 0xff, 0x00, 0x68, 0x00, 0x69, 0x00, 0x0a);
    for (const input of [le, be]) {
      const { decoded, encoded } = roundTrip(input);
      expect(decoded.ok && decoded.text).toBe('hi\n');
      expect(Array.from(encoded)).toEqual(Array.from(input));
    }
  });

  test('binary or invalid UTF-8 is refused instead of corrupted', () => {
    expect(decodeText(bytes(0x89, 0x50, 0x4e, 0x47, 0x00, 0x01)).ok).toBe(false);
    expect(decodeText(bytes(0x61, 0xc3, 0x28)).ok).toBe(false);
  });

  test('an empty file is empty UTF-8 text', () => {
    const decoded = decodeText(bytes());
    expect(decoded.ok && decoded.text).toBe('');
  });

  test('describeFormat names encoding, BOM and line ending', () => {
    expect(describeFormat({ encoding: 'utf-8', bom: false, eol: '\n' })).toBe('UTF-8, LF');
    expect(describeFormat({ encoding: 'utf-16le', bom: true, eol: '\r\n' })).toBe(
      'UTF-16 LE with BOM, CRLF'
    );
  });
});
