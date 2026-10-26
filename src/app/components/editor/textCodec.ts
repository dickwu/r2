/**
 * Decoding a stored object into editable text and encoding it back to the same
 * bytes layout: the encoding, byte-order mark and line endings the object had
 * survive an edit, so saving changes only what the user changed.
 */

export type TextEncodingName = 'utf-8' | 'utf-16le' | 'utf-16be';
export type LineEnding = '\n' | '\r\n' | '\r';

export interface TextFormat {
  encoding: TextEncodingName;
  /** Whether the object started with a byte-order mark. */
  bom: boolean;
  /** The dominant line ending, restored on save. */
  eol: LineEnding;
}

export type DecodeResult =
  { ok: true; text: string; format: TextFormat } | { ok: false; reason: string };

/** The dominant line ending in `text` ('\n' when there are no line breaks). */
export function detectLineEnding(text: string): LineEnding {
  let crlf = 0;
  let lf = 0;
  let cr = 0;
  for (let i = 0; i < text.length; i++) {
    const c = text.charCodeAt(i);
    if (c === 13) {
      if (text.charCodeAt(i + 1) === 10) {
        crlf++;
        i++;
      } else {
        cr++;
      }
    } else if (c === 10) {
      lf++;
    }
  }
  if (crlf > lf && crlf >= cr) return '\r\n';
  if (cr > lf && cr > crlf) return '\r';
  return '\n';
}

/** Text with every line break as '\n', which is what the editor works in. */
export function normalizeLineEndings(text: string): string {
  return text.replace(/\r\n?/g, '\n');
}

/** Text from the editor ('\n' breaks) with the file's own line ending. */
export function applyLineEnding(text: string, eol: LineEnding): string {
  const normalized = normalizeLineEndings(text);
  return eol === '\n' ? normalized : normalized.replace(/\n/g, eol);
}

/** NUL bytes do not occur in UTF-8 text; they mark a binary file. */
function looksBinary(bytes: Uint8Array): boolean {
  const sample = Math.min(bytes.length, 8192);
  for (let i = 0; i < sample; i++) {
    if (bytes[i] === 0) return true;
  }
  return false;
}

export function decodeText(bytes: Uint8Array): DecodeResult {
  let encoding: TextEncodingName = 'utf-8';
  let bom = false;
  let start = 0;
  if (bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf) {
    bom = true;
    start = 3;
  } else if (bytes[0] === 0xff && bytes[1] === 0xfe) {
    encoding = 'utf-16le';
    bom = true;
    start = 2;
  } else if (bytes[0] === 0xfe && bytes[1] === 0xff) {
    encoding = 'utf-16be';
    bom = true;
    start = 2;
  } else if (looksBinary(bytes)) {
    return { ok: false, reason: 'The file contains binary data, so it cannot be edited as text.' };
  }

  let text: string;
  try {
    text = new TextDecoder(encoding, { fatal: true }).decode(bytes.subarray(start));
  } catch {
    return {
      ok: false,
      reason:
        'The file is not valid UTF-8 (or UTF-16 with a byte-order mark). Editing it would corrupt characters, so it stays view-only.',
    };
  }
  const eol = detectLineEnding(text);
  return { ok: true, text: normalizeLineEndings(text), format: { encoding, bom, eol } };
}

function encodeUtf16(text: string, littleEndian: boolean): Uint8Array {
  const out = new Uint8Array(text.length * 2);
  const view = new DataView(out.buffer);
  for (let i = 0; i < text.length; i++) view.setUint16(i * 2, text.charCodeAt(i), littleEndian);
  return out;
}

/** The bytes to store for `text` (with '\n' breaks) in the file's original format. */
export function encodeText(text: string, format: TextFormat): Uint8Array {
  const withEol = applyLineEnding(text, format.eol);
  let body: Uint8Array;
  let bom: number[] = [];
  if (format.encoding === 'utf-8') {
    body = new TextEncoder().encode(withEol);
    if (format.bom) bom = [0xef, 0xbb, 0xbf];
  } else {
    const little = format.encoding === 'utf-16le';
    body = encodeUtf16(withEol, little);
    if (format.bom) bom = little ? [0xff, 0xfe] : [0xfe, 0xff];
  }
  if (bom.length === 0) return body;
  const out = new Uint8Array(bom.length + body.length);
  out.set(bom, 0);
  out.set(body, bom.length);
  return out;
}

/** Whether encoding `text` yields the bytes as plain UTF-8 text (no base64 needed). */
export function isPlainUtf8(format: TextFormat): boolean {
  return format.encoding === 'utf-8' && !format.bom;
}

export function describeFormat(format: TextFormat): string {
  const name = format.encoding.toUpperCase().replace('LE', ' LE').replace('BE', ' BE');
  const eol = format.eol === '\r\n' ? 'CRLF' : format.eol === '\r' ? 'CR' : 'LF';
  return `${name}${format.bom ? ' with BOM' : ''}, ${eol}`;
}
