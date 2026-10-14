/**
 * Picks the preview surface for a file by extension.
 *
 * - `image` / `video` / `audio`: formats the webview plays natively, streamed
 *   straight from the object URL (no full download; large videos seek).
 * - `text`: the Monaco editor, which can also save edits back to the bucket.
 * - `viewer`: everything else goes to the file-viewer pane, which downloads the
 *   object and either renders it (Office, PDF, archives, CAD, 3D, e-books,
 *   e-mail, design files, …) or reports that the format is unsupported.
 */
export type PreviewKind = 'image' | 'video' | 'audio' | 'text' | 'viewer';

/** Raster/vector formats every WebView renders in an <img>. */
export const NATIVE_IMAGE_EXTENSIONS = [
  'jpg',
  'jpeg',
  'png',
  'gif',
  'webp',
  'svg',
  'bmp',
  'ico',
  'avif',
];
export const NATIVE_VIDEO_EXTENSIONS = ['mp4', 'webm', 'ogv', 'mov', 'm4v'];
export const NATIVE_AUDIO_EXTENSIONS = ['mp3', 'wav', 'ogg', 'm4a', 'flac', 'aac', 'opus'];

export function getFileExtension(filename: string): string {
  const dot = filename.lastIndexOf('.');
  return dot === -1 ? '' : filename.slice(dot + 1).toLowerCase();
}

export function getPreviewKind(filename: string, textExtensions: readonly string[]): PreviewKind {
  const ext = getFileExtension(filename);
  if (NATIVE_IMAGE_EXTENSIONS.includes(ext)) return 'image';
  if (NATIVE_VIDEO_EXTENSIONS.includes(ext)) return 'video';
  if (NATIVE_AUDIO_EXTENSIONS.includes(ext)) return 'audio';
  if (textExtensions.includes(ext)) return 'text';
  return 'viewer';
}

/**
 * The viewer derives the format from the filename and truncates it at the
 * first `?` or `#` (URL semantics), so "Invoice #3.pdf" would lose its
 * extension. Hand it a name without those characters.
 */
export function toViewerFilename(filename: string): string {
  return filename.replace(/[?#]/g, '_');
}
