import { create } from 'zustand';
import { FileItem } from '@/app/hooks/useR2Files';

interface PreviewStore {
  file: FileItem | null;
  files: FileItem[];
  /**
   * Whether the preview (and the editor opened from it) fills the window. One
   * flag for both, so switching from preview to edit keeps the same size.
   */
  fullscreen: boolean;
  /** Whether the editor is open over the preview for `file`. */
  editing: boolean;
  /** Bumped after a save so the preview fetches the object again. */
  revision: number;
  open: (file: FileItem, files: FileItem[]) => void;
  close: () => void;
  navigate: (direction: 'prev' | 'next') => void;
  setFullscreen: (fullscreen: boolean) => void;
  toggleFullscreen: () => void;
  startEditing: () => void;
  stopEditing: () => void;
  /** Records a save: the listing entry takes the new size, the preview reloads. */
  markSaved: (size: number) => void;
}

export const usePreviewStore = create<PreviewStore>((set, get) => ({
  file: null,
  files: [],
  fullscreen: false,
  editing: false,
  revision: 0,

  open: (file, files) => set({ file, files, editing: false }),

  // Closing forgets the window state: the next preview opens at normal size.
  close: () => set({ file: null, files: [], fullscreen: false, editing: false }),

  navigate: (direction) => {
    const { file, files, editing } = get();
    // The editor holds this file's unsaved changes; it closes before moving on.
    if (!file || files.length === 0 || editing) return;

    const previewable = files.filter((f) => !f.isFolder);
    const currentIndex = previewable.findIndex((f) => f.key === file.key);
    if (currentIndex === -1 || previewable.length <= 1) return;

    const nextIndex =
      direction === 'next'
        ? (currentIndex + 1) % previewable.length
        : (currentIndex - 1 + previewable.length) % previewable.length;

    set({ file: previewable[nextIndex] });
  },

  setFullscreen: (fullscreen) => set({ fullscreen }),

  toggleFullscreen: () => set({ fullscreen: !get().fullscreen }),

  startEditing: () => {
    if (get().file) set({ editing: true });
  },

  stopEditing: () => set({ editing: false }),

  markSaved: (size) => {
    const { file, files, revision } = get();
    if (!file) return;
    const updated = { ...file, size, lastModified: new Date().toISOString() };
    set({
      file: updated,
      files: files.map((f) => (f.key === file.key ? updated : f)),
      revision: revision + 1,
    });
  },
}));
