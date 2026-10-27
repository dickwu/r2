import { create } from 'zustand';

/**
 * Which open editors hold changes not saved to the bucket yet. The editor
 * keeps its own state; it reports here so the updater can warn before a
 * restart would throw those changes away.
 */
interface EditorDirtyState {
  dirty: Record<string, true>;
  setDirty(editorId: string, dirty: boolean): void;
}

export const useEditorDirtyStore = create<EditorDirtyState>((set) => ({
  dirty: {},
  setDirty: (editorId, dirty) =>
    set((state) => {
      if (dirty === Boolean(state.dirty[editorId])) return state;
      const next = { ...state.dirty };
      if (dirty) next[editorId] = true;
      else delete next[editorId];
      return { dirty: next };
    }),
}));

export const selectUnsavedEditorCount = (state: EditorDirtyState): number =>
  Object.keys(state.dirty).length;
