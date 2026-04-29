import { useEffect, useRef } from 'react';
import { useThemeStore } from '@/app/stores/themeStore';
import { useBatchOperationStore } from '@/app/stores/batchOperationStore';

interface GlobalShortcutHandlers {
  openPalette: () => void;
  closePalette: () => void;
  paletteOpen: boolean;
  openUpload: () => void;
  refresh: () => void;
  openSettings: () => void;
  toggleView: () => void;
  viewMode: string;
}

export function useGlobalShortcuts(handlers: GlobalShortcutHandlers) {
  const handlersRef = useRef(handlers);
  handlersRef.current = handlers;

  const toggleTheme = useThemeStore((s) => s.toggleTheme);
  const toggleThemeRef = useRef(toggleTheme);
  toggleThemeRef.current = toggleTheme;

  const clearSelection = useBatchOperationStore((s) => s.clearSelection);
  const clearSelectionRef = useRef(clearSelection);
  clearSelectionRef.current = clearSelection;

  const selectedKeys = useBatchOperationStore((s) => s.selectedKeys);
  const selectedKeysRef = useRef(selectedKeys);
  selectedKeysRef.current = selectedKeys;

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement;
      const isInput =
        target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.isContentEditable;

      const mod = e.metaKey || e.ctrlKey;

      // ⌘K — Toggle command palette
      if (mod && e.key === 'k') {
        e.preventDefault();
        const h = handlersRef.current;
        if (h.paletteOpen) {
          h.closePalette();
        } else {
          h.openPalette();
        }
        return;
      }

      // ⌘U — Open upload modal (skip if input focused)
      if (mod && e.key === 'u' && !isInput) {
        e.preventDefault();
        handlersRef.current.openUpload();
        return;
      }

      // ⌘R — Refresh (palette closed, not in input)
      // Note: useKeyboardShortcuts also handles ⌘R; this adds refresh when
      // the palette is open (its keydown stops propagation) — safe to have
      // both since they call the same handler.
      if (mod && e.key === 'r' && !e.shiftKey && !isInput) {
        // Let useKeyboardShortcuts handle this to avoid double-fire.
        // Only intercept when palette is open (palette's onKeyDown won't
        // propagate to window).
        return;
      }

      // ⌘, — Open settings
      if (mod && e.key === ',') {
        e.preventDefault();
        handlersRef.current.openSettings();
        return;
      }

      // ⌘⇧T — Toggle theme
      if (mod && e.shiftKey && e.key === 'T') {
        e.preventDefault();
        toggleThemeRef.current();
        return;
      }

      // ⌘L — Toggle view list ↔ grid (skip if in input)
      if (mod && e.key === 'l' && !isInput) {
        e.preventDefault();
        handlersRef.current.toggleView();
        return;
      }

      // Esc — if palette open, palette handles it; else clear selection
      if (e.key === 'Escape' && !handlersRef.current.paletteOpen) {
        if (selectedKeysRef.current.size > 0) {
          clearSelectionRef.current();
        }
        return;
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, []);
}
