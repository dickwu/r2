import { getFileExtension, type PreviewTarget } from '@/app/components/preview/previewKind';

/** How the editor presents a text file. */
export type TextFlavor = 'code' | 'markdown' | 'json' | 'delimited';

export type Editability =
  | {
      editable: true;
      mode: 'text';
      flavor: TextFlavor;
      /** Monaco language id. */
      language: string;
      /** Field separator, for delimited (CSV/TSV) files. */
      delimiter?: ',' | '\t' | ';';
      maxBytes: number;
    }
  | {
      editable: true;
      mode: 'image';
      /** The type the edited image is encoded back to: always the original format. */
      mime: string;
      maxBytes: number;
    }
  | { editable: false; reason: string };

/** Above this, text is no longer comfortable to edit in place (or to re-upload in one PUT). */
export const MAX_TEXT_EDIT_BYTES = 5 * 1024 * 1024;
/** Images are decoded to a full-resolution canvas; this keeps that within memory. */
export const MAX_IMAGE_EDIT_BYTES = 25 * 1024 * 1024;

/** Extension → Monaco language for everything the editor treats as text. */
const LANGUAGES: Readonly<Record<string, string>> = {
  // Web
  js: 'javascript',
  mjs: 'javascript',
  cjs: 'javascript',
  jsx: 'javascript',
  ts: 'typescript',
  mts: 'typescript',
  cts: 'typescript',
  tsx: 'typescript',
  html: 'html',
  htm: 'html',
  xhtml: 'html',
  vue: 'html',
  svelte: 'html',
  astro: 'html',
  css: 'css',
  scss: 'scss',
  sass: 'scss',
  less: 'less',
  styl: 'plaintext',
  hbs: 'handlebars',
  handlebars: 'handlebars',
  mustache: 'handlebars',
  pug: 'pug',
  twig: 'twig',
  liquid: 'liquid',
  graphql: 'graphql',
  gql: 'graphql',
  // Data and config
  json: 'json',
  jsonc: 'json',
  json5: 'json',
  geojson: 'json',
  webmanifest: 'json',
  map: 'json',
  ipynb: 'json',
  har: 'json',
  ndjson: 'plaintext',
  jsonl: 'plaintext',
  yaml: 'yaml',
  yml: 'yaml',
  toml: 'ini',
  ini: 'ini',
  cfg: 'ini',
  conf: 'ini',
  properties: 'ini',
  env: 'ini',
  editorconfig: 'ini',
  xml: 'xml',
  xsd: 'xml',
  xsl: 'xml',
  xslt: 'xml',
  plist: 'xml',
  svg: 'xml',
  rss: 'xml',
  atom: 'xml',
  gpx: 'xml',
  kml: 'xml',
  csproj: 'xml',
  proto: 'protobuf',
  tf: 'hcl',
  tfvars: 'hcl',
  hcl: 'hcl',
  // Documents
  txt: 'plaintext',
  text: 'plaintext',
  md: 'markdown',
  markdown: 'markdown',
  mdx: 'markdown',
  rst: 'restructuredtext',
  adoc: 'plaintext',
  tex: 'plaintext',
  bib: 'plaintext',
  org: 'plaintext',
  log: 'plaintext',
  srt: 'plaintext',
  vtt: 'plaintext',
  diff: 'plaintext',
  patch: 'plaintext',
  csv: 'plaintext',
  tsv: 'plaintext',
  tab: 'plaintext',
  // Languages
  py: 'python',
  pyi: 'python',
  rb: 'ruby',
  go: 'go',
  rs: 'rust',
  java: 'java',
  kt: 'kotlin',
  kts: 'kotlin',
  scala: 'scala',
  groovy: 'plaintext',
  gradle: 'plaintext',
  swift: 'swift',
  m: 'objective-c',
  mm: 'objective-c',
  c: 'cpp',
  h: 'cpp',
  cc: 'cpp',
  cpp: 'cpp',
  cxx: 'cpp',
  hpp: 'cpp',
  hh: 'cpp',
  cs: 'csharp',
  fs: 'fsharp',
  fsx: 'fsharp',
  vb: 'vb',
  php: 'php',
  pl: 'perl',
  pm: 'perl',
  lua: 'lua',
  r: 'r',
  dart: 'dart',
  ex: 'elixir',
  exs: 'elixir',
  erl: 'plaintext',
  clj: 'clojure',
  cljs: 'clojure',
  edn: 'clojure',
  coffee: 'coffee',
  jl: 'julia',
  pas: 'pascal',
  scm: 'scheme',
  rkt: 'scheme',
  tcl: 'tcl',
  sol: 'sol',
  zig: 'plaintext',
  nim: 'plaintext',
  hs: 'plaintext',
  ml: 'plaintext',
  elm: 'plaintext',
  asm: 'mips',
  s: 'mips',
  sql: 'sql',
  prisma: 'plaintext',
  // Shell
  sh: 'shell',
  bash: 'shell',
  zsh: 'shell',
  fish: 'shell',
  ksh: 'shell',
  ps1: 'powershell',
  psm1: 'powershell',
  bat: 'bat',
  cmd: 'bat',
  // Repository files often stored as extensions
  gitignore: 'plaintext',
  gitattributes: 'plaintext',
  dockerignore: 'plaintext',
  npmrc: 'ini',
  lock: 'plaintext',
};

/** Extensionless file names that are text by convention. */
const TEXT_FILE_NAMES: Readonly<Record<string, string>> = {
  dockerfile: 'dockerfile',
  containerfile: 'dockerfile',
  makefile: 'plaintext',
  gnumakefile: 'plaintext',
  rakefile: 'ruby',
  gemfile: 'ruby',
  procfile: 'plaintext',
  vagrantfile: 'ruby',
  jenkinsfile: 'plaintext',
  license: 'plaintext',
  licence: 'plaintext',
  readme: 'markdown',
  changelog: 'markdown',
  authors: 'plaintext',
  contributors: 'plaintext',
  notice: 'plaintext',
  copying: 'plaintext',
  codeowners: 'plaintext',
};

/** Raster formats the editor can decode and encode back to the same format. */
const EDITABLE_IMAGES: Readonly<Record<string, string>> = {
  png: 'image/png',
  jpg: 'image/jpeg',
  jpeg: 'image/jpeg',
  jfif: 'image/jpeg',
  webp: 'image/webp',
};

/** Why each family of formats stays view-only. */
const VIEW_ONLY: ReadonlyArray<[readonly string[], string]> = [
  [['gif', 'apng'], 'Editing would flatten the animation to a single frame.'],
  [
    ['bmp', 'ico', 'avif', 'heic', 'heif', 'tif', 'tiff', 'psd', 'raw', 'cr2', 'nef', 'dng'],
    'The app can display this image format but cannot encode it back.',
  ],
  [
    [
      'mp4',
      'webm',
      'ogv',
      'mov',
      'm4v',
      'mkv',
      'avi',
      'mp3',
      'wav',
      'ogg',
      'm4a',
      'flac',
      'aac',
      'opus',
    ],
    'Audio and video are not editable in the app.',
  ],
  [['pdf'], 'PDF is a page-layout format; edit it in a PDF editor.'],
  [
    [
      'doc',
      'docx',
      'xls',
      'xlsx',
      'ppt',
      'pptx',
      'odt',
      'ods',
      'odp',
      'rtf',
      'pages',
      'numbers',
      'key',
    ],
    'Office documents are binary packages; edit them in an office suite.',
  ],
  [
    ['zip', 'gz', 'tgz', 'tar', 'bz2', 'xz', 'zst', '7z', 'rar', 'jar', 'war', 'whl'],
    'Archives are compressed binaries.',
  ],
  [
    [
      'epub',
      'mobi',
      'azw3',
      'dwg',
      'dxf',
      'stl',
      'obj',
      'glb',
      'gltf',
      'fbx',
      'parquet',
      'avro',
      'orc',
    ],
    'This binary format can be previewed but not edited.',
  ],
];

function viewOnlyReason(ext: string): string | null {
  for (const [extensions, reason] of VIEW_ONLY) {
    if (extensions.includes(ext)) return reason;
  }
  return null;
}

function baseName(name: string): string {
  const slash = name.lastIndexOf('/');
  return (slash === -1 ? name : name.slice(slash + 1)).toLowerCase();
}

/** Whether a MIME type is text the editor can open. */
export function isTextMime(mime: string | null | undefined): boolean {
  if (!mime) return false;
  const type = mime.split(';')[0].trim().toLowerCase();
  return (
    type.startsWith('text/') ||
    type.endsWith('+json') ||
    type.endsWith('+xml') ||
    [
      'application/json',
      'application/xml',
      'application/javascript',
      'application/x-javascript',
      'application/ecmascript',
      'application/x-sh',
      'application/x-yaml',
      'application/yaml',
      'application/toml',
      'application/sql',
      'application/graphql',
      'application/x-ndjson',
      'application/ld+json',
      'image/svg+xml',
    ].includes(type)
  );
}

/** The Monaco language for a file, from its extension or conventional name. */
export function monacoLanguageFor(name: string, extension = getFileExtension(name)): string {
  const byName = TEXT_FILE_NAMES[baseName(name)];
  if (byName) return byName;
  return LANGUAGES[extension] ?? 'plaintext';
}

function textEditability(name: string, extension: string, sizeError: string | null): Editability {
  if (sizeError) return { editable: false, reason: sizeError };
  const language = monacoLanguageFor(name, extension);
  if (language === 'markdown') {
    return {
      editable: true,
      mode: 'text',
      flavor: 'markdown',
      language,
      maxBytes: MAX_TEXT_EDIT_BYTES,
    };
  }
  if (language === 'json' && extension !== 'ipynb' && extension !== 'map') {
    return {
      editable: true,
      mode: 'text',
      flavor: 'json',
      language,
      maxBytes: MAX_TEXT_EDIT_BYTES,
    };
  }
  if (extension === 'csv' || extension === 'tsv' || extension === 'tab') {
    return {
      editable: true,
      mode: 'text',
      flavor: 'delimited',
      language,
      delimiter: extension === 'csv' ? ',' : '\t',
      maxBytes: MAX_TEXT_EDIT_BYTES,
    };
  }
  return { editable: true, mode: 'text', flavor: 'code', language, maxBytes: MAX_TEXT_EDIT_BYTES };
}

function tooLarge(size: number | undefined, limit: number): string | null {
  if (size === undefined || size <= limit) return null;
  const mb = (n: number) => `${(n / (1024 * 1024)).toFixed(n < 10 * 1024 * 1024 ? 1 : 0)} MB`;
  return `This file is ${mb(size)}; the editor opens files up to ${mb(limit)}. Download it to edit locally.`;
}

/**
 * Whether (and how) the editor can open a file.
 *
 * The name decides first. When the name says nothing (no or unknown extension),
 * the preview's content detection does: content the preview shows as text is
 * editable as text.
 */
export function getEditability(
  name: string,
  target: PreviewTarget | null,
  size?: number
): Editability {
  const extension = getFileExtension(name);

  const imageMime = EDITABLE_IMAGES[extension];
  if (imageMime) {
    const sizeError = tooLarge(size, MAX_IMAGE_EDIT_BYTES);
    return sizeError
      ? { editable: false, reason: sizeError }
      : { editable: true, mode: 'image', mime: imageMime, maxBytes: MAX_IMAGE_EDIT_BYTES };
  }

  const textSizeError = tooLarge(size, MAX_TEXT_EDIT_BYTES);
  if (LANGUAGES[extension] !== undefined || TEXT_FILE_NAMES[baseName(name)] !== undefined) {
    return textEditability(name, extension, textSizeError);
  }

  const reason = viewOnlyReason(extension);
  if (reason) return { editable: false, reason };

  // Unknown or missing extension: trust what the content was identified as.
  if (
    target?.kind === 'text' ||
    isTextMime(target?.detectedType) ||
    isTextMime(target?.contentType)
  ) {
    return textEditability(name, target?.extension ?? extension, textSizeError);
  }
  if (target?.kind === 'image' && target.extension && EDITABLE_IMAGES[target.extension]) {
    const sizeError = tooLarge(size, MAX_IMAGE_EDIT_BYTES);
    return sizeError
      ? { editable: false, reason: sizeError }
      : {
          editable: true,
          mode: 'image',
          mime: EDITABLE_IMAGES[target.extension],
          maxBytes: MAX_IMAGE_EDIT_BYTES,
        };
  }
  if (target?.extension) {
    const detectedReason = viewOnlyReason(target.extension);
    if (detectedReason) return { editable: false, reason: detectedReason };
  }
  return {
    editable: false,
    reason: 'This file is not a text or image format the editor supports.',
  };
}

/** The Content-Type to store a new version under when the object had none. */
export function guessContentType(name: string): string {
  const extension = getFileExtension(name);
  const known: Record<string, string> = {
    json: 'application/json',
    geojson: 'application/geo+json',
    js: 'text/javascript',
    mjs: 'text/javascript',
    ts: 'text/plain',
    html: 'text/html',
    htm: 'text/html',
    css: 'text/css',
    csv: 'text/csv',
    tsv: 'text/tab-separated-values',
    md: 'text/markdown',
    markdown: 'text/markdown',
    xml: 'application/xml',
    svg: 'image/svg+xml',
    yaml: 'application/yaml',
    yml: 'application/yaml',
    toml: 'application/toml',
    txt: 'text/plain',
    log: 'text/plain',
    sql: 'application/sql',
    png: 'image/png',
    jpg: 'image/jpeg',
    jpeg: 'image/jpeg',
    jfif: 'image/jpeg',
    webp: 'image/webp',
  };
  return (
    known[extension] ??
    (LANGUAGES[extension] !== undefined ? 'text/plain' : 'application/octet-stream')
  );
}
