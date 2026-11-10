import { create } from 'zustand';
import { createJSONStorage, persist, type PersistOptions } from 'zustand/middleware';

export type ViewMode = 'list' | 'grid' | 'column';

export const VIEW_MODES: readonly ViewMode[] = ['list', 'grid', 'column'];

interface ViewModeState {
  viewMode: ViewMode;
  setViewMode: (viewMode: ViewMode) => void;
  /** ⌘L and the command palette step list → grid → column → list. */
  cycleViewMode: () => void;
}

type PersistedViewMode = Pick<ViewModeState, 'viewMode'>;

export function isViewMode(value: unknown): value is ViewMode {
  return typeof value === 'string' && (VIEW_MODES as readonly string[]).includes(value);
}

export function nextViewMode(current: ViewMode): ViewMode {
  const index = VIEW_MODES.indexOf(current);
  return VIEW_MODES[(index + 1) % VIEW_MODES.length];
}

export const viewModePersistOptions: PersistOptions<ViewModeState, PersistedViewMode> = {
  name: 'file-view-mode',
  version: 1,
  storage: createJSONStorage(() => localStorage),
  partialize: (state) => ({ viewMode: state.viewMode }),
  // An unknown stored value (hand-edited, or from a newer build) falls back to list.
  merge: (persisted, current) => {
    const stored = (persisted as Partial<PersistedViewMode> | undefined)?.viewMode;
    return { ...current, viewMode: isViewMode(stored) ? stored : current.viewMode };
  },
};

export const useViewModeStore = create<ViewModeState>()(
  persist(
    (set, get) => ({
      viewMode: 'list',
      setViewMode: (viewMode) => set({ viewMode }),
      cycleViewMode: () => set({ viewMode: nextViewMode(get().viewMode) }),
    }),
    viewModePersistOptions
  )
);
