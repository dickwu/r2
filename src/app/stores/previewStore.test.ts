import { beforeEach, describe, expect, test } from 'bun:test';
import { usePreviewStore } from './previewStore';
import type { FileItem } from '@/app/hooks/useR2Files';

const file = (key: string, size = 10): FileItem =>
  ({ key, name: key, size, lastModified: '2026-01-01T00:00:00Z', isFolder: false }) as FileItem;

const a = file('a.md');
const b = file('b.md');

describe('previewStore fullscreen and editing', () => {
  beforeEach(() => usePreviewStore.getState().close());

  test('fullscreen carries over from the preview into the editor', () => {
    const store = usePreviewStore.getState();
    store.open(a, [a, b]);
    store.toggleFullscreen();
    store.startEditing();
    const state = usePreviewStore.getState();
    expect(state.editing).toBe(true);
    expect(state.fullscreen).toBe(true);
    // Leaving fullscreen in the editor keeps the editor open.
    state.setFullscreen(false);
    expect(usePreviewStore.getState().editing).toBe(true);
    // And closing the editor keeps the preview's size.
    state.setFullscreen(true);
    state.stopEditing();
    expect(usePreviewStore.getState().fullscreen).toBe(true);
    expect(usePreviewStore.getState().file?.key).toBe('a.md');
  });

  test('closing the preview resets fullscreen and editing', () => {
    const store = usePreviewStore.getState();
    store.open(a, [a]);
    store.setFullscreen(true);
    store.startEditing();
    store.close();
    const state = usePreviewStore.getState();
    expect(state.fullscreen).toBe(false);
    expect(state.editing).toBe(false);
    expect(state.file).toBeNull();
  });

  test('cannot move to another file while editing', () => {
    const store = usePreviewStore.getState();
    store.open(a, [a, b]);
    store.startEditing();
    store.navigate('next');
    expect(usePreviewStore.getState().file?.key).toBe('a.md');
    store.stopEditing();
    store.navigate('next');
    expect(usePreviewStore.getState().file?.key).toBe('b.md');
  });

  test('editing needs an open file', () => {
    usePreviewStore.getState().startEditing();
    expect(usePreviewStore.getState().editing).toBe(false);
  });

  test('markSaved updates the entry and asks the preview to reload', () => {
    const store = usePreviewStore.getState();
    store.open(a, [a, b]);
    const before = usePreviewStore.getState().revision;
    store.markSaved(1234);
    const state = usePreviewStore.getState();
    expect(state.revision).toBe(before + 1);
    expect(state.file?.size).toBe(1234);
    expect(state.files.find((f) => f.key === 'a.md')?.size).toBe(1234);
    expect(state.files.find((f) => f.key === 'b.md')?.size).toBe(10);
    expect(state.file?.lastModified === '2026-01-01T00:00:00Z').toBe(false);
  });
});
