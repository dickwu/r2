export type RenameMode = 'overwrite' | 'auto-rename';

const COMPOUND_EXT_RE = /\.tar\.(gz|bz2|xz|zst|lz)$/;

function formatTimestamp(date: Date): string {
  const y = date.getFullYear();
  const mo = String(date.getMonth() + 1).padStart(2, '0');
  const d = String(date.getDate()).padStart(2, '0');
  const h = String(date.getHours()).padStart(2, '0');
  const mi = String(date.getMinutes()).padStart(2, '0');
  const s = String(date.getSeconds()).padStart(2, '0');
  const ms = String(date.getMilliseconds()).padStart(3, '0');
  return `${y}${mo}${d}-${h}${mi}${s}-${ms}`;
}

/**
 * Rename a file to avoid S3 key collisions.
 *
 * Handles relative paths (e.g., "images/photo.jpg"), dotfiles,
 * compound extensions (.tar.gz), and files with no extension.
 *
 * @param fileName - original filename, may include directory prefix
 * @param mode - 'overwrite' returns unchanged, 'auto-rename' appends timestamp
 * @param now - optional Date for testability (defaults to new Date())
 */
export function renameKey(fileName: string, mode: RenameMode, now?: Date): string {
  if (mode === 'overwrite' || !fileName) return fileName;

  const lastSlash = fileName.lastIndexOf('/');
  const dir = lastSlash >= 0 ? fileName.slice(0, lastSlash + 1) : '';
  const basename = lastSlash >= 0 ? fileName.slice(lastSlash + 1) : fileName;

  if (!basename) return fileName;

  const ts = formatTimestamp(now ?? new Date());

  const compoundMatch = basename.match(COMPOUND_EXT_RE);
  if (compoundMatch) {
    const extStart = basename.lastIndexOf(`.tar.${compoundMatch[1]}`);
    const name = basename.slice(0, extStart);
    const ext = basename.slice(extStart);
    return `${dir}${name}-${ts}${ext}`;
  }

  if (basename.startsWith('.') && !basename.slice(1).includes('.')) {
    return `${dir}${basename}-${ts}`;
  }

  const lastDot = basename.lastIndexOf('.');
  if (lastDot <= 0) {
    return `${dir}${basename}-${ts}`;
  }

  const name = basename.slice(0, lastDot);
  const ext = basename.slice(lastDot);
  return `${dir}${name}-${ts}${ext}`;
}
