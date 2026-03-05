import { create } from 'zustand';
import { FileItem } from '@/app/hooks/useR2Files';

interface PreviewStore {
  file: FileItem | null;
  files: FileItem[];
  open: (file: FileItem, files: FileItem[]) => void;
  close: () => void;
  navigate: (direction: 'prev' | 'next') => void;
}

export const usePreviewStore = create<PreviewStore>((set, get) => ({
  file: null,
  files: [],

  open: (file, files) => set({ file, files }),

  close: () => set({ file: null, files: [] }),

  navigate: (direction) => {
    const { file, files } = get();
    if (!file || files.length === 0) return;

    const previewable = files.filter((f) => !f.isFolder);
    const currentIndex = previewable.findIndex((f) => f.key === file.key);
    if (currentIndex === -1 || previewable.length <= 1) return;

    const nextIndex =
      direction === 'next'
        ? (currentIndex + 1) % previewable.length
        : (currentIndex - 1 + previewable.length) % previewable.length;

    set({ file: previewable[nextIndex] });
  },
}));
