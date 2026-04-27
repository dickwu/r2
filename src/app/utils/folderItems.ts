export interface FileItem {
  name: string;
  key: string;
  isFolder: boolean;
  size?: number;
  lastModified?: string;
}

export interface StoredFolderFile {
  key: string;
  size: number;
  lastModified: string;
}

export interface FolderContents {
  files: StoredFolderFile[];
  folders: string[];
}

export interface LazyPrefixFile {
  key: string;
  name: string;
  size: number;
  last_modified: string;
}

export interface LazyPrefixResult {
  files: LazyPrefixFile[];
  folders: string[];
  prefix: string;
  from_cache: boolean;
}

export interface ListPrefixOptions {
  forceRefresh?: boolean;
}

export interface LoadFolderItemsResult {
  items: FileItem[];
  source: 'prefix' | 'cache-fallback' | 'all-cache-fallback';
}

interface LoadFolderItemsOptions<Config> {
  config: Config;
  prefix: string;
  forceRefresh?: boolean;
  readCachedFolder: (prefix: string) => Promise<FolderContents>;
  readAllCachedFiles?: () => Promise<StoredFolderFile[]>;
  readPrefixFolder: (
    config: Config,
    prefix: string,
    options?: ListPrefixOptions
  ) => Promise<LazyPrefixResult>;
}

const nameCollator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

function extractName(key: string, prefix: string): string {
  const relativePath = prefix ? key.slice(prefix.length) : key;
  return relativePath.replace(/\/$/, '');
}

function hasFolderContents(contents: FolderContents): boolean {
  return contents.files.length > 0 || contents.folders.length > 0;
}

function lazyFilesToStored(files: LazyPrefixFile[]): StoredFolderFile[] {
  return files.map((file) => ({
    key: file.key,
    size: file.size,
    lastModified: file.last_modified,
  }));
}

function buildFolderContentsFromAllFiles(
  files: StoredFolderFile[],
  prefix: string
): FolderContents {
  const directFiles: StoredFolderFile[] = [];
  const folders = new Set<string>();

  for (const file of files) {
    if (prefix && !file.key.startsWith(prefix)) continue;
    if (file.key === prefix || file.key.endsWith('/')) continue;

    const relativePath = prefix ? file.key.slice(prefix.length) : file.key;
    if (!relativePath) continue;

    const slashIndex = relativePath.indexOf('/');
    if (slashIndex === -1) {
      directFiles.push(file);
      continue;
    }

    folders.add(`${prefix}${relativePath.slice(0, slashIndex + 1)}`);
  }

  return {
    files: directFiles,
    folders: Array.from(folders),
  };
}

export function buildFileItems(
  files: StoredFolderFile[],
  folders: string[],
  prefix: string
): FileItem[] {
  const items: FileItem[] = [];

  for (const folder of folders) {
    if (folder === '/' || folder === '') continue;
    items.push({
      name: extractName(folder, prefix),
      key: folder,
      isFolder: true,
    });
  }

  for (const file of files) {
    if (file.key === prefix || file.key.endsWith('/')) continue;
    items.push({
      name: extractName(file.key, prefix),
      key: file.key,
      isFolder: false,
      size: file.size,
      lastModified: file.lastModified,
    });
  }

  return items.sort((a, b) => {
    if (a.isFolder && !b.isFolder) return -1;
    if (!a.isFolder && b.isFolder) return 1;
    return nameCollator.compare(a.name, b.name);
  });
}

export async function loadFolderItems<Config>({
  config,
  prefix,
  forceRefresh = false,
  readCachedFolder,
  readAllCachedFiles,
  readPrefixFolder,
}: LoadFolderItemsOptions<Config>): Promise<LoadFolderItemsResult> {
  const cached = await readCachedFolder(prefix);

  try {
    const prefixResult = await readPrefixFolder(config, prefix, { forceRefresh });
    return {
      items: buildFileItems(lazyFilesToStored(prefixResult.files), prefixResult.folders, prefix),
      source: 'prefix',
    };
  } catch (err) {
    if (hasFolderContents(cached)) {
      return {
        items: buildFileItems(cached.files, cached.folders, prefix),
        source: 'cache-fallback',
      };
    }

    if (readAllCachedFiles) {
      const allCachedContents = buildFolderContentsFromAllFiles(await readAllCachedFiles(), prefix);
      if (hasFolderContents(allCachedContents)) {
        return {
          items: buildFileItems(allCachedContents.files, allCachedContents.folders, prefix),
          source: 'all-cache-fallback',
        };
      }
    }

    throw err;
  }
}
