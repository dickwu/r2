import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { clearSessionLog, recentSessionLog } from '@/app/lib/diagnostics/sessionLog';
import { appendPrefillText, clampPrefillText, useReportStore } from './reportStore';

const state = () => useReportStore.getState();

describe('reportStore', () => {
  beforeEach(() => {
    useReportStore.setState({ isOpen: false, prefill: null });
    clearSessionLog();
  });
  afterEach(() => clearSessionLog());

  test('opens blank without a prefill and logs the moment', () => {
    state().open();
    expect(state().isOpen).toBe(true);
    expect(state().prefill).toBeNull();
    expect(recentSessionLog().entries.map((entry) => entry.msg)).toEqual([
      'Report a problem opened',
    ]);
  });

  test('carries a prefill until close resets it', () => {
    state().open({ title: 'Upload failed: a.bin', description: 'Upload failed — a.bin' });
    expect(state().prefill).toEqual({
      title: 'Upload failed: a.bin',
      description: 'Upload failed — a.bin',
    });
    state().close();
    expect(state().isOpen).toBe(false);
    expect(state().prefill).toBeNull();
  });

  test('a later blank open does not inherit an earlier prefill', () => {
    state().open({ title: 'Sync failed: media' });
    state().close();
    state().open();
    expect(state().prefill).toBeNull();
  });
});

describe('clampPrefillText', () => {
  test('passes text within the limit and blanks a missing value', () => {
    expect(clampPrefillText('abc', 3)).toBe('abc');
    expect(clampPrefillText(undefined, 3)).toBe('');
    expect(clampPrefillText('', 3)).toBe('');
  });

  test('cuts text to the limit', () => {
    expect(clampPrefillText('abcdef', 4)).toBe('abcd');
  });

  test('never leaves half of a surrogate pair at the cut', () => {
    const text = 'abc\u{1F600}def';
    expect(clampPrefillText(text, 4)).toBe('abc');
    expect(clampPrefillText(text, 5)).toBe('abc\u{1F600}');
    expect(encodeURIComponent(clampPrefillText(text, 4))).toBe('abc');
  });
});

describe('appendPrefillText', () => {
  test('fills a blank draft and leaves it alone without new text', () => {
    expect(appendPrefillText('  ', 'Upload failed — a.bin', 100)).toBe('Upload failed — a.bin');
    expect(appendPrefillText('my notes', undefined, 100)).toBe('my notes');
    expect(appendPrefillText('my notes', '', 100)).toBe('my notes');
  });

  test('adds new text below a written draft', () => {
    expect(appendPrefillText('my notes', 'details', 100)).toBe('my notes\n\ndetails');
  });

  test('cuts only the appended text at the limit, never the draft', () => {
    expect(appendPrefillText('my notes', 'details', 12)).toBe('my notes\n\nde');
  });
});
