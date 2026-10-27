import { compareVersions, isVersion, normalizeVersion } from './releaseNotes';

/**
 * What the updater remembers across launches, in localStorage:
 * - the version whose dialog already opened by itself, so each version
 *   interrupts once (the status bar keeps showing it);
 * - the version the user chose to skip;
 * - the last version this install ran, so the first launch after an update,
 *   in-app or by hand, shows What's new once.
 */

export const UPDATE_PREF_KEYS = {
  prompted: 'r2.update.promptedVersion',
  skipped: 'r2.update.skippedVersion',
  lastRun: 'r2.update.lastRunVersion',
} as const;

export interface PrefStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

const memory = new Map<string, string>();
const memoryStorage: PrefStorage = {
  getItem: (key) => memory.get(key) ?? null,
  setItem: (key, value) => void memory.set(key, value),
  removeItem: (key) => void memory.delete(key),
};

const defaultStorage = (): PrefStorage => {
  try {
    if (typeof window !== 'undefined' && window.localStorage) return window.localStorage;
  } catch {
    // Storage blocked: fall back to this session only.
  }
  return memoryStorage;
};

let storageOverride: PrefStorage | null = null;

/** Tests swap in their own storage; null restores localStorage. */
export const setPrefStorage = (storage: PrefStorage | null): void => {
  storageOverride = storage;
};

const storage = (): PrefStorage => storageOverride ?? defaultStorage();

const read = (key: string): string | null => {
  try {
    const value = storage().getItem(key);
    return isVersion(value) ? normalizeVersion(value) : null;
  } catch {
    return null;
  }
};

const write = (key: string, value: string | null): void => {
  try {
    if (value === null) storage().removeItem(key);
    else storage().setItem(key, normalizeVersion(value));
  } catch {
    // A preference that can't be saved only means the dialog may open again.
  }
};

export const skippedVersion = (): string | null => read(UPDATE_PREF_KEYS.skipped);

/** Whether a background check may open the dialog for `version` by itself. */
export const shouldAutoPrompt = (version: string): boolean => {
  const v = normalizeVersion(version);
  const prompted = read(UPDATE_PREF_KEYS.prompted);
  const skipped = read(UPDATE_PREF_KEYS.skipped);
  return v !== prompted && v !== skipped;
};

export const markPrompted = (version: string): void => write(UPDATE_PREF_KEYS.prompted, version);

export const skipVersion = (version: string): void => write(UPDATE_PREF_KEYS.skipped, version);

export const clearSkippedVersion = (): void => write(UPDATE_PREF_KEYS.skipped, null);

/**
 * Records that `current` is running and returns the version it replaced when
 * this is the first launch after an upgrade. Null on a fresh install (nothing
 * recorded yet), on a relaunch of the same version, and after a downgrade.
 */
export const takeUpgradedFrom = (current: string): string | null => {
  const previous = read(UPDATE_PREF_KEYS.lastRun);
  write(UPDATE_PREF_KEYS.lastRun, current);
  if (!previous || compareVersions(current, previous) <= 0) return null;
  return previous;
};

/** For the dev test seam: pretend the last launch ran `version` (or forget it). */
export const setLastRunVersion = (version: string | null): void =>
  write(UPDATE_PREF_KEYS.lastRun, version);

export const clearUpdatePrefs = (): void => {
  for (const key of Object.values(UPDATE_PREF_KEYS)) {
    try {
      storage().removeItem(key);
    } catch {
      // ignore
    }
  }
};
