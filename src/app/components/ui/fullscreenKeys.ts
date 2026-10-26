/** The parts of a keydown the fullscreen shortcuts look at. */
export interface FullscreenKey {
  key: string;
  defaultPrevented: boolean;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  repeat?: boolean;
  target: EventTarget | null;
}

/** Inputs, text areas, contenteditable regions and Monaco take typed letters themselves. */
export function isTypingTarget(target: EventTarget | null): boolean {
  if (!target || typeof (target as Element).closest !== 'function') return false;
  const element = target as HTMLElement;
  if (element.isContentEditable) return true;
  return !!element.closest('input, textarea, select, [contenteditable="true"], .monaco-editor');
}

/** Whether a dialog drawn above the modals (antd confirm and friends) has the focus. */
function isInAntdDialog(target: EventTarget | null): boolean {
  if (!target || typeof (target as Element).closest !== 'function') return false;
  return !!(target as Element).closest('.ant-modal-root, .ant-modal-wrap');
}

/**
 * Whether Escape should leave fullscreen. Only leaves it: the caller claims the
 * key (preventDefault), so the modal's own Escape-to-close stands down and the
 * editor never closes, and never drops unsaved changes, on that press.
 *
 * A key some overlay already claimed (a lightbox, Monaco's suggestions, an antd
 * confirm) is that overlay's.
 */
export function shouldExitFullscreen(event: FullscreenKey, fullscreen: boolean): boolean {
  return (
    fullscreen && event.key === 'Escape' && !event.defaultPrevented && !isInAntdDialog(event.target)
  );
}

/** Whether the key is the bare `F` that toggles fullscreen (never while typing). */
export function isFullscreenToggleKey(event: FullscreenKey): boolean {
  return (
    (event.key === 'f' || event.key === 'F') &&
    !event.metaKey &&
    !event.ctrlKey &&
    !event.altKey &&
    !event.repeat &&
    !event.defaultPrevented &&
    !isTypingTarget(event.target) &&
    !isInAntdDialog(event.target)
  );
}
