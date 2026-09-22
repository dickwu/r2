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

type FrameScheduler = (callback: () => void) => unknown;
type FrameCanceller = (handle: unknown) => void;

interface LoadFolderItemsOptions<Config> {
  config: Config;
  prefix: string;
  signal?: AbortSignal;
  forceRefresh?: boolean;
  fallbackSnapshot?: FolderSnapshot | null;
  readCachedFolder: (
    config: Config,
    prefix: string,
    options: {
      signal?: AbortSignal;
      onUpdate: (snapshot: FolderSnapshot) => void;
    }
  ) => Promise<FolderSnapshot | null>;
  readPrefixFolder: (
    config: Config,
    prefix: string,
    options: {
      signal?: AbortSignal;
      forceRefresh?: boolean;
      onUpdate: (snapshot: FolderSnapshot) => void;
    }
  ) => Promise<FolderSnapshot>;
  onUpdate: (snapshot: FolderSnapshot) => void;
  scheduleFrame?: FrameScheduler;
  cancelFrame?: FrameCanceller;
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

function defaultScheduleFrame(callback: () => void): unknown {
  if (typeof requestAnimationFrame === 'function') return requestAnimationFrame(callback);
  return setTimeout(callback, 0);
}

function defaultCancelFrame(handle: unknown) {
  if (typeof cancelAnimationFrame === 'function' && typeof handle === 'number') {
    cancelAnimationFrame(handle);
    return;
  }
  clearTimeout(handle as ReturnType<typeof setTimeout>);
}

export function createFolderUpdatePublisher(
  onUpdate: (snapshot: FolderSnapshot) => void,
  options: {
    signal?: AbortSignal;
    scheduleFrame?: FrameScheduler;
    cancelFrame?: FrameCanceller;
  } = {}
) {
  const scheduleFrame = options.scheduleFrame ?? defaultScheduleFrame;
  const cancelFrame = options.cancelFrame ?? defaultCancelFrame;
  let publishedFirst = false;
  let pending: FolderSnapshot | null = null;
  let scheduled: unknown = null;

  const emit = (snapshot: FolderSnapshot) => {
    if (options.signal?.aborted) return;
    onUpdate(snapshot);
  };
  const clearScheduled = () => {
    if (scheduled !== null) {
      cancelFrame(scheduled);
      scheduled = null;
    }
  };
  const flush = () => {
    const snapshot = pending;
    pending = null;
    clearScheduled();
    if (snapshot) emit(snapshot);
  };
  const publish = (snapshot: FolderSnapshot, options: { flush?: boolean } = {}) => {
    if (options.flush || snapshot.complete) {
      pending = null;
      clearScheduled();
      emit(snapshot);
      publishedFirst = true;
      return;
    }
    if (!publishedFirst) {
      emit(snapshot);
      publishedFirst = true;
      return;
    }
    pending = snapshot;
    if (scheduled === null) {
      scheduled = scheduleFrame(() => {
        scheduled = null;
        flush();
      });
    }
  };
  const abort = () => {
    pending = null;
    clearScheduled();
  };
  options.signal?.addEventListener('abort', abort, { once: true });
  return { publish, flush, abort };
}

function createWarmOverlay(base: FolderSnapshot | null | undefined) {
  if (!base?.complete) return (update: FolderSnapshot) => update;
  let items = base.items;
  const indexes = new Map(items.map((item, index) => [item.key, index]));
  return (update: FolderSnapshot): FolderSnapshot => {
    if (update.complete) return update;
    let next = items;
    const additions: FileItem[] = [];
    for (const item of update.items) {
      const index = indexes.get(item.key);
      if (index === undefined) {
        indexes.set(item.key, next.length + additions.length);
        additions.push(item);
        continue;
      }
      if (next === items) next = items.slice();
      next[index] = item;
    }
    if (additions.length > 0) {
      next = next === items ? items.slice() : next;
      for (const item of additions) next.push(item);
      next.sort(compareFileItems);
      indexes.clear();
      next.forEach((item, index) => indexes.set(item.key, index));
    }
    items = next;
    return {
      items,
      complete: false,
      fromCache: base.fromCache,
      freshness: 'partial',
    };
  };
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
  forceRefresh,
  fallbackSnapshot,
  readCachedFolder,
  readPrefixFolder,
  onUpdate,
  scheduleFrame,
  cancelFrame,
}: LoadFolderItemsOptions<Config>): Promise<FolderSnapshot> {
  signal?.throwIfAborted();
  const publisher = createFolderUpdatePublisher(onUpdate, { signal, scheduleFrame, cancelFrame });
  const cacheOverlay = createWarmOverlay(fallbackSnapshot);
  // A cache read failure should not prevent the live provider request.
  const cached = forceRefresh
    ? null
    : await readCachedFolder(config, prefix, {
        signal,
        onUpdate: (snapshot) => publisher.publish(cacheOverlay(snapshot)),
      }).catch(() => null);
  signal?.throwIfAborted();
  if (cached) {
    const cachedSnapshot = cacheOverlay(cached);
    publisher.publish(cachedSnapshot);
    if (cached.complete && cached.freshness === 'fresh') return cachedSnapshot;
  }
  const liveOverlay = createWarmOverlay(cached ?? fallbackSnapshot);
  try {
    const result = await readPrefixFolder(config, prefix, {
      signal,
      forceRefresh: true,
      onUpdate: (snapshot) => publisher.publish(liveOverlay(snapshot)),
    });
    publisher.flush();
    return result;
  } catch (error) {
    publisher.flush();
    throw error;
  }
}
