/**
 * Picks the preview surface for a file by extension, or by what its content
 * is (sniffed by the backend) when its name carries no extension a preview
 * handles.
 *
 * - `image` / `video` / `audio`: formats the webview plays natively, streamed
 *   straight from the object URL (no full download; large videos seek).
 * - `text`: the Monaco editor, which can also save edits back to the bucket.
 * - `viewer`: everything else goes to the file-viewer pane, which downloads the
 *   object and either renders it (Office, PDF, archives, CAD, 3D, e-books,
 *   e-mail, design files, …) or reports that the format is unsupported.
 */
export type PreviewKind = 'image' | 'video' | 'audio' | 'text' | 'viewer';

/** Where to preview an object, and as which format. */
export interface PreviewTarget {
  kind: PreviewKind;
  /**
   * The format to render: the name's extension, or the one the Content-Type
   * stands for. Empty when only the type's family (`image/*`, …) is known.
   */
  extension: string;
  /** The object's Content-Type, set when its content (not the name) chose the preview. */
  contentType?: string;
}

/** What the backend identified an object as (`detect_url_type`). */
export interface DetectedType {
  /** MIME type identified from the content's first bytes. */
  mime: string;
  /** Extension that type is stored under, without the dot ('' if unknown). */
  extension: string;
  /** Human-readable name of the type. */
  name: string;
  /** The Content-Type header the object was stored with. */
  contentType: string | null;
}

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

/**
 * The extension each Content-Type stands for, for when the stored type has to
 * say what the content alone cannot (see `getDetectedPreview`). Every value is
 * a format some preview handles; types left out fall back on their family
 * (`image/*`, `video/*`, …).
 */
export const CONTENT_TYPE_EXTENSIONS: Readonly<Record<string, string>> = {
  // Images: native first, then the ones only the file viewer decodes.
  'image/jpeg': 'jpg',
  'image/jpg': 'jpg',
  'image/pjpeg': 'jpg',
  'image/png': 'png',
  'image/apng': 'png',
  'image/gif': 'gif',
  'image/webp': 'webp',
  'image/svg+xml': 'svg',
  'image/bmp': 'bmp',
  'image/x-ms-bmp': 'bmp',
  'image/x-icon': 'ico',
  'image/vnd.microsoft.icon': 'ico',
  'image/avif': 'avif',
  'image/tiff': 'tiff',
  'image/heic': 'heic',
  'image/heif': 'heif',
  'image/jxl': 'jxl',
  'image/vnd.adobe.photoshop': 'psd',
  'application/dicom': 'dcm',
  // Video and audio.
  'video/mp4': 'mp4',
  'video/webm': 'webm',
  'video/ogg': 'ogv',
  'video/quicktime': 'mov',
  'video/x-m4v': 'm4v',
  'audio/mpeg': 'mp3',
  'audio/mp3': 'mp3',
  'audio/wav': 'wav',
  'audio/wave': 'wav',
  'audio/x-wav': 'wav',
  'audio/vnd.wave': 'wav',
  'audio/ogg': 'ogg',
  'audio/mp4': 'm4a',
  'audio/x-m4a': 'm4a',
  'audio/flac': 'flac',
  'audio/x-flac': 'flac',
  'audio/aac': 'aac',
  'audio/x-aac': 'aac',
  'audio/opus': 'opus',
  'audio/midi': 'mid',
  'audio/x-midi': 'mid',
  // Text and code.
  'text/plain': 'txt',
  'text/markdown': 'md',
  'text/x-markdown': 'md',
  'text/csv': 'csv',
  'text/tab-separated-values': 'tsv',
  'text/html': 'html',
  'text/css': 'css',
  'text/javascript': 'js',
  'application/javascript': 'js',
  'application/x-javascript': 'js',
  'application/typescript': 'ts',
  'application/json': 'json',
  'text/xml': 'xml',
  'application/xml': 'xml',
  'text/yaml': 'yaml',
  'text/x-yaml': 'yaml',
  'application/yaml': 'yaml',
  'application/x-yaml': 'yaml',
  'application/toml': 'toml',
  'application/sql': 'sql',
  'application/x-sh': 'sh',
  'text/x-shellscript': 'sh',
  'application/x-ipynb+json': 'ipynb',
  // Documents.
  'application/pdf': 'pdf',
  'application/msword': 'doc',
  'application/vnd.openxmlformats-officedocument.wordprocessingml.document': 'docx',
  'application/vnd.ms-excel': 'xls',
  'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet': 'xlsx',
  'application/vnd.ms-powerpoint': 'ppt',
  'application/vnd.openxmlformats-officedocument.presentationml.presentation': 'pptx',
  'application/vnd.openxmlformats-officedocument.presentationml.slideshow': 'ppsx',
  'application/vnd.oasis.opendocument.text': 'odt',
  'application/vnd.oasis.opendocument.spreadsheet': 'ods',
  'application/vnd.oasis.opendocument.presentation': 'odp',
  'application/vnd.apple.pages': 'pages',
  'application/vnd.apple.numbers': 'numbers',
  'application/vnd.apple.keynote': 'key',
  'application/rtf': 'rtf',
  'text/rtf': 'rtf',
  'application/epub+zip': 'epub',
  'application/vnd.ms-xpsdocument': 'xps',
  'message/rfc822': 'eml',
  'application/vnd.ms-outlook': 'msg',
  'application/mbox': 'mbox',
  // Archives.
  'application/zip': 'zip',
  'application/x-zip-compressed': 'zip',
  'application/x-7z-compressed': '7z',
  'application/vnd.rar': 'rar',
  'application/x-rar-compressed': 'rar',
  'application/x-tar': 'tar',
  'application/gzip': 'gz',
  'application/x-gzip': 'gz',
  'application/x-bzip2': 'bz2',
  'application/x-xz': 'xz',
  'application/zstd': 'zst',
  'application/java-archive': 'jar',
  'application/vnd.android.package-archive': 'apk',
  'application/x-iso9660-image': 'iso',
  // 3D models, drawings and maps.
  'model/gltf-binary': 'glb',
  'model/gltf+json': 'gltf',
  'model/obj': 'obj',
  'model/stl': 'stl',
  'model/3mf': '3mf',
  'model/vnd.usdz+zip': 'usdz',
  'image/vnd.dxf': 'dxf',
  'image/vnd.dwg': 'dwg',
  'application/geo+json': 'geojson',
  'application/vnd.google-earth.kml+xml': 'kml',
  'application/vnd.google-earth.kmz': 'kmz',
  'application/gpx+xml': 'gpx',
  // Fonts and data.
  'font/ttf': 'ttf',
  'font/otf': 'otf',
  'font/woff': 'woff',
  'font/woff2': 'woff2',
  'application/vnd.sqlite3': 'sqlite',
  'application/x-sqlite3': 'sqlite',
  'application/vnd.apache.parquet': 'parquet',
  'application/wasm': 'wasm',
};

/**
 * Detections that only name a family or container (plain text, a ZIP or OLE
 * file, unknown or empty data), which the stored Content-Type may refine.
 */
const GENERIC_DETECTIONS = [
  'application/octet-stream',
  'application/x-empty',
  'text/plain',
  'application/zip',
  'application/x-ole-storage',
];

/** Extensions that name the same format. */
const FORMAT_ALIASES: Readonly<Record<string, string>> = {
  jpeg: 'jpg',
  tif: 'tiff',
  htm: 'html',
  yml: 'yaml',
  midi: 'mid',
  markdown: 'md',
};

function getMimeEssence(type: string): string {
  return type.split(';')[0].trim().toLowerCase();
}

export function getFileExtension(filename: string): string {
  const dot = filename.lastIndexOf('.');
  return dot === -1 ? '' : filename.slice(dot + 1).toLowerCase();
}

function getExtensionPreviewKind(
  extension: string,
  textExtensions: readonly string[]
): PreviewKind {
  if (NATIVE_IMAGE_EXTENSIONS.includes(extension)) return 'image';
  if (NATIVE_VIDEO_EXTENSIONS.includes(extension)) return 'video';
  if (NATIVE_AUDIO_EXTENSIONS.includes(extension)) return 'audio';
  if (textExtensions.includes(extension)) return 'text';
  return 'viewer';
}

export function getPreviewKind(filename: string, textExtensions: readonly string[]): PreviewKind {
  return getExtensionPreviewKind(getFileExtension(filename), textExtensions);
}

/** The preview a format calls for, given as its extension. */
export function getExtensionPreview(
  extension: string,
  textExtensions: readonly string[]
): PreviewTarget {
  return { kind: getExtensionPreviewKind(extension, textExtensions), extension };
}

/**
 * The preview an object's Content-Type calls for, or null when it names no
 * format a preview handles (`application/octet-stream`, a missing header, …).
 * Parameters such as `charset` are ignored; `+json` and `+xml` types open as
 * JSON and XML text.
 */
export function getContentTypePreview(
  contentType: string | null | undefined,
  textExtensions: readonly string[]
): PreviewTarget | null {
  if (!contentType) return null;
  const mimeType = getMimeEssence(contentType);
  const [family, subtype] = mimeType.split('/');
  if (!family || !subtype) return null;

  const extension =
    CONTENT_TYPE_EXTENSIONS[mimeType] ??
    (subtype.endsWith('+json') ? 'json' : subtype.endsWith('+xml') ? 'xml' : '');
  if (extension) {
    return { kind: getExtensionPreviewKind(extension, textExtensions), extension, contentType };
  }
  // Unlisted subtypes still open where their family plays.
  switch (family) {
    case 'image':
    case 'video':
    case 'audio':
    case 'text':
      return { kind: family, extension: '', contentType };
    default:
      return null;
  }
}

/**
 * The preview an object's content calls for, or null when nothing renders it.
 * The detected type decides when it is specific, routed by its extension just
 * like a correct file name; when the bytes only name a family or container,
 * the stored Content-Type may refine them. The target keeps the stored type
 * (or else the detected one) for saving edits back.
 */
export function getDetectedPreview(
  detected: DetectedType,
  textExtensions: readonly string[],
  isViewerFormat: (extension: string) => boolean
): PreviewTarget | null {
  const renders = (target: PreviewTarget | null) =>
    target && (target.kind !== 'viewer' || isViewerFormat(target.extension)) ? target : null;
  const fromBytes = renders(getExtensionPreview(detected.extension, textExtensions));
  const generic = GENERIC_DETECTIONS.includes(getMimeEssence(detected.mime));
  const target =
    (!generic && fromBytes) ||
    renders(getContentTypePreview(detected.contentType, textExtensions)) ||
    fromBytes;
  return target && { ...target, contentType: detected.contentType ?? detected.mime };
}

/**
 * Whether two previews render an object the same way: the native players and
 * the editor take any format of their kind, the viewer renders the format it
 * is told.
 */
export function isSameRendering(a: PreviewTarget, b: PreviewTarget): boolean {
  if (a.kind !== b.kind) return false;
  const format = (extension: string) => FORMAT_ALIASES[extension] ?? extension;
  return a.kind !== 'viewer' || format(a.extension) === format(b.extension);
}

/**
 * The viewer derives the format from the filename and truncates it at the
 * first `?` or `#` (URL semantics), so "Invoice #3.pdf" would lose its
 * extension. Hand it a name without those characters, ending in `extension`
 * when one is given: parts of the viewer (its toolbar) read the format from
 * the name even when an explicit `type` is passed.
 */
export function toViewerFilename(filename: string, extension?: string): string {
  const name = filename.replace(/[?#]/g, '_');
  return extension && getFileExtension(name) !== extension ? `${name}.${extension}` : name;
}
