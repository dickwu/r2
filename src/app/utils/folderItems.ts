import { observeFolderAccumulation } from '@/app/lib/folderTiming';

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

export interface FolderRequestScope {
  provider: string;
  account_id: string;
  bucket: string;
  prefix: string;
  request_id: string;
  generation: number;
}

export type FolderFreshness = 'fresh' | 'stale' | 'partial';

export interface FolderPage extends LazyPrefixResult, FolderRequestScope {
  page_index: number;
  next_cursor: string | null;
  complete: boolean;
  freshness: FolderFreshness;
}

export interface FolderSnapshot {
  items: FileItem[];
  complete: boolean;
  fromCache: boolean;
  freshness: FolderFreshness;
}

interface LoadFolderItemsOptions<Config> {
  config: Config;
  prefix: string;
  signal?: AbortSignal;
  readCachedFolder: (config: Config, prefix: string) => Promise<FolderSnapshot | null>;
  readPrefixFolder: (
    config: Config,
    prefix: string,
    options: {
      signal?: AbortSignal;
      onUpdate: (snapshot: FolderSnapshot) => void;
    }
  ) => Promise<FolderSnapshot>;
  onUpdate: (snapshot: FolderSnapshot) => void;
}

const nameCollator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

function extractName(key: string, prefix: string): string {
  const relativePath = prefix ? key.slice(prefix.length) : key;
  return relativePath.replace(/\/$/, '');
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

  return items.sort(compareFileItems);
}

function compareFileItems(a: FileItem, b: FileItem): number {
  if (a.isFolder && !b.isFolder) return -1;
  if (!a.isFolder && b.isFolder) return 1;
  return nameCollator.compare(a.name, b.name);
}

/** Match the entire navigation identity before accepting any payload. */
export function matchesFolderRequest(
  expected: FolderRequestScope,
  page: FolderRequestScope
): boolean {
  return (
    expected.provider === page.provider &&
    expected.account_id === page.account_id &&
    expected.bucket === page.bucket &&
    expected.prefix === page.prefix &&
    expected.request_id === page.request_id &&
    expected.generation === page.generation
  );
}

export function createFolderPageAccumulator(scope: FolderRequestScope) {
  const files = new Map<string, FileItem>();
  const folders = new Set<string>();
  let sorted: FileItem[] = [];
  const cursors = new Set<string>();
  let nextIndex = 0;
  let complete = false;

  return {
    accept(page: FolderPage): FolderSnapshot | null {
      if (!matchesFolderRequest(scope, page)) return null;
      if (complete) throw new Error('Folder page received after listing is complete');
      if (page.page_index !== nextIndex) throw new Error('Folder page sequence is incomplete');
      if (!page.complete && (!page.next_cursor || cursors.has(page.next_cursor))) {
        throw new Error('Folder listing returned a missing or repeated cursor');
      }
      if (page.complete && page.next_cursor) throw new Error('Complete folder page has a cursor');
      if (page.next_cursor) cursors.add(page.next_cursor);
      nextIndex += 1;
      complete = page.complete;
      return observeFolderAccumulation(scope, page.page_index, () => {
        const newKeys = new Set<string>();
        const replacements = new Map<string, FileItem>();
        const additions: FileItem[] = [];
        for (const file of page.files) {
          if (file.key === scope.prefix || file.key.endsWith('/')) continue;
          const previous = files.get(file.key);
          if (
            previous &&
            previous.size === file.size &&
            previous.lastModified === file.last_modified
          )
            continue;
          const item: FileItem = {
            name: extractName(file.key, scope.prefix),
            key: file.key,
            isFolder: false,
            size: file.size,
            lastModified: file.last_modified,
          };
          files.set(file.key, item);
          if (!previous) newKeys.add(file.key);
          else replacements.set(file.key, item);
        }
        for (const folder of page.folders) {
          if (!folder || folder === '/' || folders.has(folder)) continue;
          folders.add(folder);
          additions.push({ name: extractName(folder, scope.prefix), key: folder, isFolder: true });
        }
        // Read the final value after duplicate keys within this page were replaced.
        for (const key of newKeys) additions.push(files.get(key)!);
        additions.sort(compareFileItems);
        if (additions.length > 0 || replacements.size > 0) {
          const merged = new Array<FileItem>(sorted.length + additions.length);
          let oldIndex = 0;
          let writeIndex = 0;
          const copyExisting = (end: number) => {
            while (oldIndex < end) {
              const previous = sorted[oldIndex++];
              merged[writeIndex++] = previous.isFolder
                ? previous
                : (replacements.get(previous.key) ?? previous);
            }
          };
          for (const addition of additions) {
            // Find a whole existing run to copy. Binary upper bounds avoid an
            // expensive Intl comparison for every old row on every new page.
            let low = oldIndex;
            let high = sorted.length;
            if (low < high && compareFileItems(sorted[low], addition) <= 0) {
              if (compareFileItems(sorted[high - 1], addition) <= 0) low = high;
              else {
                while (low < high) {
                  const middle = low + Math.floor((high - low) / 2);
                  if (compareFileItems(sorted[middle], addition) <= 0) low = middle + 1;
                  else high = middle;
                }
              }
            }
            // Existing equal names precede newly observed equal names, exactly
            // matching the previous stable sort over Map/Set insertion order.
            copyExisting(low);
            merged[writeIndex++] = addition;
          }
          copyExisting(sorted.length);
          sorted = merged;
        }
        // Never mutate older arrays or their item objects after publishing.
        return {
          items: sorted,
          complete,
          fromCache: page.from_cache,
          freshness: complete ? page.freshness : 'partial',
        };
      });
    },
  };
}

/** Publish known cache immediately. Failed refreshes remain errors, even with cached rows. */
export async function loadFolderItems<Config>({
  config,
  prefix,
  signal,
  readCachedFolder,
  readPrefixFolder,
  onUpdate,
}: LoadFolderItemsOptions<Config>): Promise<FolderSnapshot> {
  signal?.throwIfAborted();
  // A cache read failure should not prevent the live provider request.
  const cached = await readCachedFolder(config, prefix).catch(() => null);
  signal?.throwIfAborted();
  if (cached) onUpdate(cached);
  return readPrefixFolder(config, prefix, { signal, onUpdate });
}
