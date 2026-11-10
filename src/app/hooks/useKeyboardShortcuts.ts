import { useEffect, useEffectEvent } from 'react';

interface KeyboardShortcutHandlers {
  onSelectAll?: () => void;
  onDelete?: () => void;
  onRefresh?: () => void;
  onUpload?: () => void;
  onEscape?: () => void;
  onFocusSearch?: () => void;
}

export function useKeyboardShortcuts(handlers: KeyboardShortcutHandlers, enabled: boolean = true) {
  const getHandlers = useEffectEvent(() => handlers);

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
        getHandlers().onSelectAll?.();
        return;
      }

      // Cmd+F or / — Focus search
      if ((mod && e.key === 'f') || (e.key === '/' && !isInput)) {
        e.preventDefault();
        getHandlers().onFocusSearch?.();
        return;
      }

      // Cmd+R — Refresh (prevent browser reload in dev)
      if (mod && e.key === 'r' && !e.shiftKey) {
        e.preventDefault();
        getHandlers().onRefresh?.();
        return;
      }

      // Delete/Backspace — Delete selected (only when not in an input)
      if ((e.key === 'Delete' || e.key === 'Backspace') && !isInput) {
        e.preventDefault();
        getHandlers().onDelete?.();
        return;
      }

      // Escape — Clear selection or close
      if (e.key === 'Escape') {
        getHandlers().onEscape?.();
        return;
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [enabled]);
}
