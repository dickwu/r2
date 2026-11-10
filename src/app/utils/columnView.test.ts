import { describe, expect, test } from 'bun:test';
import type { FileItem } from '@/app/utils/folderItems';
import {
  columnKeyAction,
  columnPrefixes,
  highlightedKey,
  pickItem,
  sortColumnItems,
  visibleFileSelection,
  type ColumnData,
} from './columnView';

const folder = (key: string): FileItem => ({
  key,
  name: key.replace(/\/$/, '').split('/').pop()!,
  isFolder: true,
});
const file = (key: string): FileItem => ({
  key,
  name: key.split('/').pop()!,
  isFolder: false,
  size: 1,
});

// bucket: photos/{2023/,2024/{a.jpg,b.jpg}}, docs/, readme.md
const root: ColumnData = {
  prefix: '',
  items: [folder('docs/'), folder('photos/'), file('readme.md')],
};
const photos: ColumnData = {
  prefix: 'photos/',
  items: [folder('photos/2023/'), folder('photos/2024/')],
};
const y2024: ColumnData = {
  prefix: 'photos/2024/',
  items: [file('photos/2024/a.jpg'), file('photos/2024/b.jpg')],
};

describe('columnPrefixes', () => {
  test('root is a single column', () => {
    expect(columnPrefixes('')).toEqual(['']);
  });
  test('every ancestor of the current folder is a column', () => {
    expect(columnPrefixes('photos/2024/')).toEqual(['', 'photos/', 'photos/2024/']);
  });
  test('a path without a trailing slash still yields folder prefixes', () => {
    expect(columnPrefixes('photos/2024')).toEqual(['', 'photos/', 'photos/2024/']);
  });
});

describe('sortColumnItems', () => {
  test('folders first, then natural case-insensitive name order', () => {
    const sorted = sortColumnItems([
      file('b10.txt'),
      folder('Zeta/'),
      file('B2.txt'),
      folder('alpha/'),
      file('a.txt'),
    ]);
    expect(sorted.map((i) => i.name)).toEqual(['alpha', 'Zeta', 'a.txt', 'B2.txt', 'b10.txt']);
  });
  test('does not mutate the listing', () => {
    const items = [file('b'), file('a')];
    sortColumnItems(items);
    expect(items.map((i) => i.name)).toEqual(['b', 'a']);
  });
});

describe('selection and highlight', () => {
  const pick = { parent: 'photos/2024/', file: y2024.items[1] };

  test('a picked file shows only while its folder is the last column', () => {
    expect(visibleFileSelection(pick, 'photos/2024/')).toBe(pick);
    expect(visibleFileSelection(pick, 'photos/')).toBeNull();
    expect(visibleFileSelection(null, '')).toBeNull();
  });

  test('ancestor columns highlight the opened folder; the last highlights the file', () => {
    const prefixes = columnPrefixes('photos/2024/');
    expect(highlightedKey(prefixes, 0, pick)).toBe('photos/');
    expect(highlightedKey(prefixes, 1, pick)).toBe('photos/2024/');
    expect(highlightedKey(prefixes, 2, pick)).toBe('photos/2024/b.jpg');
    expect(highlightedKey(prefixes, 2, null)).toBeNull();
  });

  test('picking a folder opens it; picking a file selects it in its column', () => {
    expect(pickItem(folder('docs/'), '')).toEqual({ type: 'navigate', path: 'docs/' });
    expect(pickItem(file('readme.md'), '')).toEqual({
      type: 'selectFile',
      selection: { parent: '', file: file('readme.md') },
    });
  });
});

describe('columnKeyAction', () => {
  const trail = [root, photos, y2024];

  test('at the root with nothing picked, Down picks the first row', () => {
    expect(columnKeyAction([root], '', null, 'ArrowDown')).toEqual({
      type: 'navigate',
      path: 'docs/',
    });
    expect(columnKeyAction([root], '', null, 'ArrowUp')).toBeNull();
    expect(columnKeyAction([root], '', null, 'ArrowLeft')).toBeNull();
  });

  test('Up/Down on the open folder moves to its siblings', () => {
    expect(columnKeyAction(trail, 'photos/2024/', null, 'ArrowUp')).toEqual({
      type: 'navigate',
      path: 'photos/2023/',
    });
    expect(columnKeyAction(trail, 'photos/2024/', null, 'ArrowDown')).toBeNull();
    expect(columnKeyAction([root, photos], 'photos/', null, 'ArrowDown')).toEqual({
      type: 'selectFile',
      selection: { parent: '', file: file('readme.md') },
    });
  });

  test('Right enters the open folder, Left goes back to the parent', () => {
    expect(columnKeyAction(trail, 'photos/2024/', null, 'ArrowRight')).toEqual({
      type: 'selectFile',
      selection: { parent: 'photos/2024/', file: y2024.items[0] },
    });
    expect(columnKeyAction(trail, 'photos/2024/', null, 'ArrowLeft')).toEqual({
      type: 'navigate',
      path: 'photos/',
    });
    expect(columnKeyAction([root, photos], 'photos/', null, 'ArrowRight')).toEqual({
      type: 'navigate',
      path: 'photos/2023/',
    });
  });

  test('with a file picked, Up/Down step through files, Left clears it, Enter opens it', () => {
    const pick = { parent: 'photos/2024/', file: y2024.items[0] };
    expect(columnKeyAction(trail, 'photos/2024/', pick, 'ArrowDown')).toEqual({
      type: 'selectFile',
      selection: { parent: 'photos/2024/', file: y2024.items[1] },
    });
    expect(columnKeyAction(trail, 'photos/2024/', pick, 'ArrowUp')).toBeNull();
    expect(columnKeyAction(trail, 'photos/2024/', pick, 'ArrowLeft')).toEqual({
      type: 'clearFile',
    });
    expect(columnKeyAction(trail, 'photos/2024/', pick, 'Enter')).toEqual({
      type: 'open',
      file: y2024.items[0],
    });
  });

  test('a file picked at the root has no parent to step back to', () => {
    const pick = { parent: '', file: file('readme.md') };
    expect(columnKeyAction([root], '', pick, 'ArrowLeft')).toBeNull();
    expect(columnKeyAction([root], '', pick, 'ArrowUp')).toEqual({
      type: 'navigate',
      path: 'photos/',
    });
  });

  test('a stale pick from another folder is ignored', () => {
    const stale = { parent: 'docs/', file: file('docs/x.txt') };
    expect(columnKeyAction(trail, 'photos/2024/', stale, 'Enter')).toBeNull();
  });
});
