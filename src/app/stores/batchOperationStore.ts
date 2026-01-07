import { create } from 'zustand';

interface BatchOperationStore {
  // Selection state
  selectedKeys: Set<string>;

  // Delete modal state
  deleteModalOpen: boolean;
  keysToDelete: Set<string>;
  isDeleting: boolean;

  // Move modal state
  moveModalOpen: boolean;
  keysToMove: Set<string>;
  isMoving: boolean;

  // Selection actions
  toggleSelection: (key: string) => void;
  selectAll: (keys: string[]) => void;
  clearSelection: () => void;

  // Delete modal actions
  openDeleteModal: (keys: Set<string>) => void;
  closeDeleteModal: () => void;
  setDeleting: (isDeleting: boolean) => void;

  // Move modal actions
  openMoveModal: (keys: Set<string>) => void;
  closeMoveModal: () => void;
  setMoving: (isMoving: boolean) => void;

  // Reset all state (e.g., when bucket changes)
  reset: () => void;
}

const initialState = {
  selectedKeys: new Set<string>(),
  deleteModalOpen: false,
  keysToDelete: new Set<string>(),
  isDeleting: false,
  moveModalOpen: false,
  keysToMove: new Set<string>(),
  isMoving: false,
};

export const useBatchOperationStore = create<BatchOperationStore>((set) => ({
  ...initialState,

  // Selection actions
  toggleSelection: (key) =>
    set((state) => {
      const next = new Set(state.selectedKeys);
      if (next.has(key)) {
        next.delete(key);
      } else {
        next.add(key);
      }
      return { selectedKeys: next };
    }),

  selectAll: (keys) => set({ selectedKeys: new Set(keys) }),

  clearSelection: () => set({ selectedKeys: new Set() }),

  // Delete modal actions
  openDeleteModal: (keys) =>
    set({
      keysToDelete: keys,
      deleteModalOpen: true,
    }),

  closeDeleteModal: () => set({ deleteModalOpen: false }),

  setDeleting: (isDeleting) => set({ isDeleting }),

  // Move modal actions
  openMoveModal: (keys) =>
    set({
      keysToMove: keys,
      moveModalOpen: true,
    }),

  closeMoveModal: () => set({ moveModalOpen: false }),

  setMoving: (isMoving) => set({ isMoving }),

  // Reset all state
  reset: () => set(initialState),
}));

// Selectors
export const selectHasFolders = (state: BatchOperationStore) =>
  Array.from(state.selectedKeys).some((key) => key.endsWith('/'));
