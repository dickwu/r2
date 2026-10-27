import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import {
  markPrompted,
  setLastRunVersion,
  setPrefStorage,
  shouldAutoPrompt,
  skipVersion,
  takeUpgradedFrom,
  type PrefStorage,
} from './updatePrefs';

const map = new Map<string, string>();
const storage: PrefStorage = {
  getItem: (k) => map.get(k) ?? null,
  setItem: (k, v) => void map.set(k, v),
  removeItem: (k) => void map.delete(k),
};

describe('updatePrefs', () => {
  beforeEach(() => {
    map.clear();
    setPrefStorage(storage);
  });
  afterEach(() => setPrefStorage(null));

  test('the dialog opens by itself once per version', () => {
    expect(shouldAutoPrompt('0.3.18')).toBe(true);
    markPrompted('0.3.18');
    expect(shouldAutoPrompt('0.3.18')).toBe(false);
    expect(shouldAutoPrompt('0.3.19')).toBe(true);
  });

  test('a skipped version never opens it by itself', () => {
    skipVersion('v0.3.18');
    expect(shouldAutoPrompt('0.3.18')).toBe(false);
    expect(shouldAutoPrompt('0.3.19')).toBe(true);
  });

  test("What's new only after an upgrade", () => {
    expect(takeUpgradedFrom('0.3.17')).toBeNull(); // fresh install
    expect(takeUpgradedFrom('0.3.17')).toBeNull(); // same version again
    expect(takeUpgradedFrom('0.3.18')).toBe('0.3.17');
    expect(takeUpgradedFrom('0.3.18')).toBeNull(); // only once
    setLastRunVersion('0.3.19');
    expect(takeUpgradedFrom('0.3.18')).toBeNull(); // downgrade
  });

  test('ignores garbage and storage that throws', () => {
    map.set('r2.update.skippedVersion', 'not a version');
    expect(shouldAutoPrompt('0.3.18')).toBe(true);
    setPrefStorage({
      getItem: () => {
        throw new Error('blocked');
      },
      setItem: () => {
        throw new Error('blocked');
      },
      removeItem: () => {},
    });
    expect(shouldAutoPrompt('0.3.18')).toBe(true);
    expect(() => markPrompted('0.3.18')).not.toThrow();
  });
});
