import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { create } from 'zustand';

interface CacheUpdatedEvent {
  action: 'delete' | 'move' | 'update';
  affected_paths: string[];
}

interface PathsRemovedEvent {
  removed_paths: string[];
}

interface PathsCreatedEvent {
  created_paths: string[];
}

interface CurrentPathStore {
  currentPath: string;
  cacheUpdatedPaths: string[];
  removedPaths: string[];
  createdPaths: string[];
  setCurrentPath: (path: string) => void;
  goToParent: () => void;
  reset: () => void;
}

function getParentPath(path: string): string {
  if (!path) return '';
  const withoutTrailing = path.endsWith('/') ? path.slice(0, -1) : path;
  const lastSlash = withoutTrailing.lastIndexOf('/');
  if (lastSlash === -1) return '';
  return `${withoutTrailing.slice(0, lastSlash + 1)}`;
}

export const useCurrentPathStore = create<CurrentPathStore>((set, get) => ({
  currentPath: '',
  cacheUpdatedPaths: [],
  removedPaths: [],
  createdPaths: [],
  setCurrentPath: (path) => set({ currentPath: path }),
  goToParent: () => {
    const { currentPath } = get();
    set({ currentPath: getParentPath(currentPath) });
  },
  reset: () => set({ currentPath: '' }),
}));

let unlistenCacheUpdated: UnlistenFn | undefined;
let unlistenPathsRemoved: UnlistenFn | undefined;
let unlistenPathsCreated: UnlistenFn | undefined;

async function ensureEventListeners() {
  if (unlistenCacheUpdated && unlistenPathsRemoved && unlistenPathsCreated) return;

  if (!unlistenCacheUpdated) {
    unlistenCacheUpdated = await listen<CacheUpdatedEvent>('cache-updated', (event) => {
      const { affected_paths } = event.payload;
      useCurrentPathStore.setState({ cacheUpdatedPaths: [...affected_paths] });
    });
  }

  if (!unlistenPathsRemoved) {
    unlistenPathsRemoved = await listen<PathsRemovedEvent>('paths-removed', (event) => {
      const { removed_paths } = event.payload;
      useCurrentPathStore.setState({ removedPaths: [...removed_paths] });

      const { currentPath, setCurrentPath } = useCurrentPathStore.getState();
      if (removed_paths.includes(currentPath)) {
        const removedSet = new Set(removed_paths);
        let nextPath = currentPath;
        while (nextPath && removedSet.has(nextPath)) {
          nextPath = getParentPath(nextPath);
        }
        setCurrentPath(nextPath);
      }
    });
  }

  if (!unlistenPathsCreated) {
    unlistenPathsCreated = await listen<PathsCreatedEvent>('paths-created', (event) => {
      const { created_paths } = event.payload;
      useCurrentPathStore.setState({ createdPaths: [...created_paths] });
    });
  }
}

if (typeof window !== 'undefined') {
  void ensureEventListeners();
}
