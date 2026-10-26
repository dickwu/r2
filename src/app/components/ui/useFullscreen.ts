'use client';

import { useCallback, useEffect, useRef } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { usePreviewStore } from '@/app/stores/previewStore';
import { isFullscreenToggleKey, shouldExitFullscreen } from './fullscreenKeys';

interface UseFullscreenOptions {
  /** Whether this window's keys are live (off while another window is on top). */
  active?: boolean;
  /** Whether the bare `F` key toggles fullscreen here. */
  shortcut?: boolean;
}

/**
 * The fullscreen state the file preview and the file editor share: one flag in
 * the preview store, so opening the editor from a fullscreen preview stays
 * fullscreen, and closing the preview resets it.
 *
 * Escape leaves fullscreen and nothing else; the next Escape is the window's.
 */
export function useFullscreen({ active = true, shortcut = false }: UseFullscreenOptions = {}) {
  const fullscreen = usePreviewStore((s) => s.fullscreen);
  const setFullscreen = usePreviewStore((s) => s.setFullscreen);
  const toggle = usePreviewStore((s) => s.toggleFullscreen);

  useEffect(() => {
    if (!active) return;
    // On window, in the bubble phase: after the modals' document listener (which
    // waits a tick and skips a key claimed by then) and after any overlay that
    // claims the key first.
    const onKeyDown = (event: KeyboardEvent) => {
      if (shouldExitFullscreen(event, usePreviewStore.getState().fullscreen)) {
        event.preventDefault();
        setFullscreen(false);
        return;
      }
      if (shortcut && isFullscreenToggleKey(event)) {
        event.preventDefault();
        toggle();
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [active, shortcut, setFullscreen, toggle]);

  const exit = useCallback(() => setFullscreen(false), [setFullscreen]);
  return { fullscreen, toggle, exit };
}

/**
 * Takes the app window itself fullscreen while `fullscreen` is on, and puts it
 * back as it was afterwards (a window the user had made fullscreen stays so).
 * Outside Tauri, or without the permission, the in-window fullscreen remains.
 */
export function useWindowFullscreen(fullscreen: boolean) {
  const restore = useRef<boolean | null>(null);

  useEffect(() => {
    let cancelled = false;
    const apply = async () => {
      try {
        const appWindow = getCurrentWindow();
        if (fullscreen) {
          const was = await appWindow.isFullscreen();
          if (cancelled) return;
          restore.current = was;
          if (!was) await appWindow.setFullscreen(true);
        } else if (restore.current === false) {
          restore.current = null;
          await appWindow.setFullscreen(false);
        }
      } catch {
        // Not running in Tauri (or not allowed): the modal still fills the window.
      }
    };
    void apply();
    return () => {
      cancelled = true;
    };
  }, [fullscreen]);

  // Leaving the preview while fullscreen hands the window back as well.
  useEffect(
    () => () => {
      if (restore.current === false) {
        restore.current = null;
        getCurrentWindow()
          .setFullscreen(false)
          .catch(() => undefined);
      }
    },
    []
  );
}
