import { describe, expect, test } from 'bun:test';
import {
  isFullscreenToggleKey,
  isTypingTarget,
  shouldExitFullscreen,
  type FullscreenKey,
} from './fullscreenKeys';

/** A stand-in element: `closest` answers whether it sits inside `inside`. */
function element(inside: string[] = [], editable = false): EventTarget {
  return {
    isContentEditable: editable,
    closest: (selector: string) =>
      selector.split(',').some((s) => inside.includes(s.trim())) ? {} : null,
  } as unknown as EventTarget;
}

const key = (k: string, patch: Partial<FullscreenKey> = {}): FullscreenKey => ({
  key: k,
  defaultPrevented: false,
  metaKey: false,
  ctrlKey: false,
  altKey: false,
  repeat: false,
  target: element(),
  ...patch,
});

describe('shouldExitFullscreen', () => {
  test('Escape leaves fullscreen only while fullscreen', () => {
    expect(shouldExitFullscreen(key('Escape'), true)).toBe(true);
    expect(shouldExitFullscreen(key('Escape'), false)).toBe(false);
    expect(shouldExitFullscreen(key('Enter'), true)).toBe(false);
  });

  test('Escape typed in the editor leaves fullscreen (and nothing else)', () => {
    // Monaco has nothing open: the key is unclaimed, so it exits fullscreen;
    // the caller claims it so the editor modal does not close.
    expect(shouldExitFullscreen(key('Escape', { target: element(['.monaco-editor']) }), true)).toBe(
      true
    );
  });

  test('an Escape an overlay already handled is left to it', () => {
    expect(shouldExitFullscreen(key('Escape', { defaultPrevented: true }), true)).toBe(false);
    expect(
      shouldExitFullscreen(key('Escape', { target: element(['.ant-modal-wrap']) }), true)
    ).toBe(false);
  });
});

describe('isFullscreenToggleKey', () => {
  test('a bare F toggles', () => {
    expect(isFullscreenToggleKey(key('f'))).toBe(true);
    expect(isFullscreenToggleKey(key('F'))).toBe(true);
  });

  test('not with modifiers, repeats, or while typing', () => {
    expect(isFullscreenToggleKey(key('f', { metaKey: true }))).toBe(false);
    expect(isFullscreenToggleKey(key('f', { ctrlKey: true }))).toBe(false);
    expect(isFullscreenToggleKey(key('f', { repeat: true }))).toBe(false);
    expect(isFullscreenToggleKey(key('f', { target: element(['input']) }))).toBe(false);
    expect(isFullscreenToggleKey(key('f', { target: element(['.monaco-editor']) }))).toBe(false);
    expect(isFullscreenToggleKey(key('f', { target: element([], true) }))).toBe(false);
  });

  test('isTypingTarget ignores non-elements', () => {
    expect(isTypingTarget(null)).toBe(false);
    expect(isTypingTarget({} as EventTarget)).toBe(false);
  });
});
