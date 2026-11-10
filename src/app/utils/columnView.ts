/**
 * Pure helpers for the Finder-style column view.
 *
 * The column view is driven by the same `currentPath` as the list and grid
 * views: every ancestor of the current folder is one column, and the current
 * folder's contents are the last column. Opening a folder in any column sets
 * `currentPath` to it, which drops the columns to its right. A file selected
 * in a column is remembered together with the folder it lives in, so it only
 * shows while that folder is the last column.
 */
import type { FileItem } from '@/app/utils/folderItems';
import { parentPrefix } from '@/app/utils/syncScope';

export interface ColumnData {
  prefix: string;
  items: FileItem[];
}

/** A file picked in the column view, scoped to the folder it was picked in. */
export interface ColumnFileSelection {
  parent: string;
  file: FileItem;
}

export type ColumnKeyAction =
  | { type: 'navigate'; path: string }
  | { type: 'selectFile'; selection: ColumnFileSelection }
  | { type: 'clearFile' }
  | { type: 'open'; file: FileItem };

export type ColumnKey = 'ArrowUp' | 'ArrowDown' | 'ArrowLeft' | 'ArrowRight' | 'Enter';

const nameCollator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

/** '' → [''], 'a/b/' → ['', 'a/', 'a/b/']. Every entry is a folder prefix. */
export function columnPrefixes(currentPath: string): string[] {
  const prefixes = [''];
  if (!currentPath) return prefixes;
  const parts = currentPath.split('/').filter(Boolean);
  let acc = '';
  for (const part of parts) {
    acc += `${part}/`;
    prefixes.push(acc);
  }
  return prefixes;
}

/** Folders first, then a natural, case-insensitive name order (as Finder sorts). */
export function sortColumnItems(items: FileItem[]): FileItem[] {
  return [...items].sort((a, b) => {
    if (a.isFolder !== b.isFolder) return a.isFolder ? -1 : 1;
    return nameCollator.compare(a.name, b.name);
  });
}

/** The selected file, but only while the folder it was picked in is the last column. */
export function visibleFileSelection(
  selection: ColumnFileSelection | null,
  currentPath: string
): ColumnFileSelection | null {
  return selection && selection.parent === currentPath ? selection : null;
}

/**
 * Key of the highlighted row in column `index`: the folder opened from it, or
 * — in the last column — the selected file.
 */
export function highlightedKey(
  prefixes: string[],
  index: number,
  selection: ColumnFileSelection | null
): string | null {
  if (index < prefixes.length - 1) return prefixes[index + 1];
  if (selection && selection.parent === prefixes[index]) return selection.file.key;
  return null;
}

/** The action for picking `item` in the column listing `prefix`. */
export function pickItem(item: FileItem, prefix: string): ColumnKeyAction {
  if (item.isFolder) return { type: 'navigate', path: item.key };
  return { type: 'selectFile', selection: { parent: prefix, file: item } };
}

/**
 * Keyboard navigation, Finder style. The cursor is the selected file if there
 * is one, otherwise the open folder (highlighted in the column before last).
 * Up/Down move within the cursor's column, Right enters the open folder,
 * Left steps back to the parent, Enter opens the selected file.
 */
export function columnKeyAction(
  columns: ColumnData[],
  currentPath: string,
  selection: ColumnFileSelection | null,
  key: ColumnKey
): ColumnKeyAction | null {
  const visible = visibleFileSelection(selection, currentPath);
  const last = columns[columns.length - 1];

  if (visible) {
    const items = last?.items ?? [];
    const index = items.findIndex((item) => item.key === visible.file.key);
    switch (key) {
      case 'Enter':
        return { type: 'open', file: visible.file };
      case 'ArrowLeft':
        return currentPath ? { type: 'clearFile' } : null;
      case 'ArrowUp':
      case 'ArrowDown': {
        const next = stepItem(items, index, key === 'ArrowUp' ? -1 : 1);
        return next ? pickItem(next, last.prefix) : null;
      }
      default:
        return null;
    }
  }

  if (!currentPath) {
    // Root with nothing picked: Down/Right pick the first row.
    if (key !== 'ArrowDown' && key !== 'ArrowRight') return null;
    const first = last?.items[0];
    return first ? pickItem(first, last.prefix) : null;
  }

  // The cursor is the open folder, listed in its parent's column.
  const parent = parentPrefix(currentPath);
  const parentColumn = columns.find((column) => column.prefix === parent);
  switch (key) {
    case 'ArrowLeft':
      return { type: 'navigate', path: parent };
    case 'ArrowRight': {
      const first = last?.prefix === currentPath ? last.items[0] : undefined;
      return first ? pickItem(first, currentPath) : null;
    }
    case 'ArrowUp':
    case 'ArrowDown': {
      const items = parentColumn?.items ?? [];
      const index = items.findIndex((item) => item.key === currentPath);
      const next = stepItem(items, index, key === 'ArrowUp' ? -1 : 1);
      return next ? pickItem(next, parent) : null;
    }
    default:
      return null;
  }
}

function stepItem(items: FileItem[], index: number, delta: number): FileItem | null {
  if (items.length === 0) return null;
  if (index < 0) return items[delta > 0 ? 0 : items.length - 1];
  const next = index + delta;
  if (next < 0 || next >= items.length) return null;
  return items[next];
}
