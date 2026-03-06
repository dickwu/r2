import { useEffect, useRef } from 'react';

interface KeyboardShortcutHandlers {
  onSelectAll?: () => void;
  onDelete?: () => void;
  onRefresh?: () => void;
  onUpload?: () => void;
  onEscape?: () => void;
  onFocusSearch?: () => void;
}

export function useKeyboardShortcuts(handlers: KeyboardShortcutHandlers, enabled: boolean = true) {
  const handlersRef = useRef(handlers);
  handlersRef.current = handlers;

  useEffect(() => {
    if (!enabled) return;

    const handleKeyDown = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement;
      const isInput =
        target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.isContentEditable;

      const mod = e.metaKey || e.ctrlKey;

      // Cmd+A — Select all (only when not in an input)
      if (mod && e.key === 'a' && !isInput) {
        e.preventDefault();
        handlersRef.current.onSelectAll?.();
        return;
      }

      // Cmd+F or / — Focus search
      if ((mod && e.key === 'f') || (e.key === '/' && !isInput)) {
        e.preventDefault();
        handlersRef.current.onFocusSearch?.();
        return;
      }

      // Cmd+R — Refresh (prevent browser reload in dev)
      if (mod && e.key === 'r' && !e.shiftKey) {
        e.preventDefault();
        handlersRef.current.onRefresh?.();
        return;
      }

      // Delete/Backspace — Delete selected (only when not in an input)
      if ((e.key === 'Delete' || e.key === 'Backspace') && !isInput) {
        e.preventDefault();
        handlersRef.current.onDelete?.();
        return;
      }

      // Escape — Clear selection or close
      if (e.key === 'Escape') {
        handlersRef.current.onEscape?.();
        return;
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [enabled]);
}
