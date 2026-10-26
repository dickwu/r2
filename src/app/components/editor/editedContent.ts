/**
 * The bytes an edit produces, shared by "Save changes" (upload) and "Download
 * edited copy" (a local file), so both always write exactly the same thing.
 */
import { applyLineEnding, encodeText, isPlainUtf8, type TextFormat } from './textCodec';
import type { SaveErrorKind } from './saveFlow';

export interface EditedText {
  /** The exact bytes of the file, in its original encoding, BOM and line endings. */
  bytes: Uint8Array;
  /** What to upload: a string for plain UTF-8 (cheaper over IPC), else the bytes. */
  body: string | Uint8Array;
}

/** Editor text ('\n' breaks) as the stored file: same encoding, BOM and line endings. */
export function editedText(text: string, format: TextFormat): EditedText {
  const bytes = encodeText(text, format);
  return { bytes, body: isPlainUtf8(format) ? applyLineEnding(text, format.eol) : bytes };
}

/**
 * Why encoded image bytes cannot stand in for the original file, or null when
 * they can. WebKit falls back to PNG for formats it cannot write; those bytes
 * would sit under a .webp/.jpg name.
 */
export function imageEncodingProblem(blobType: string, mime: string): string | null {
  if (blobType === mime) return null;
  return `This system cannot write ${mime.replace('image/', '').toUpperCase()} images.`;
}

/** The name a downloaded copy is offered under: the object's own file name. */
export function copyFileName(key: string): string {
  const name = key.slice(key.lastIndexOf('/') + 1).trim();
  return name || 'edited-file';
}

/** Save-dialog filters keeping the original extension first. */
export function copyFilters(fileName: string): { name: string; extensions: string[] }[] {
  const dot = fileName.lastIndexOf('.');
  if (dot <= 0 || dot === fileName.length - 1) return [];
  const ext = fileName.slice(dot + 1);
  return [{ name: `${ext.toUpperCase()} file`, extensions: [ext] }];
}

export interface CopyDependencies {
  /** Asks where to save; null when the user cancels. */
  pickPath: (
    defaultPath: string,
    filters: { name: string; extensions: string[] }[]
  ) => Promise<string | null>;
  writeFile: (path: string, bytes: Uint8Array) => Promise<void>;
}

export type CopyOutcome = { status: 'saved'; path: string } | { status: 'cancelled' };

/** Writes `bytes` to a file the user picks, offered under the object's name. */
export async function downloadEditedCopy(
  key: string,
  bytes: Uint8Array,
  deps: CopyDependencies
): Promise<CopyOutcome> {
  const name = copyFileName(key);
  const path = await deps.pickPath(name, copyFilters(name));
  if (!path) return { status: 'cancelled' };
  await deps.writeFile(path, bytes);
  return { status: 'saved', path };
}

/** The file name at the end of a local path, for "Saved a copy to …". */
export function localFileName(path: string): string {
  return path.split(/[\\/]/).pop() || path;
}

export type SaveFailureKind = SaveErrorKind | 'conflict' | 'size' | 'encode';

export interface SaveFailure {
  text: string;
  /** Offer "Download edited copy": the edit is fine, only storing it failed. */
  offerCopy: boolean;
  /** The credentials cannot write here: make the download the main action. */
  readOnly: boolean;
}

/** What the editor shows after a save fails, and which way out it offers. */
export function saveFailure(kind: SaveFailureKind, text: string): SaveFailure {
  return { text, offerCopy: kind !== 'encode', readOnly: kind === 'forbidden' };
}
