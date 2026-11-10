import { beforeEach, describe, expect, test } from 'bun:test';
import {
  isViewMode,
  nextViewMode,
  useViewModeStore,
  viewModePersistOptions as options,
} from './viewModeStore';

const store = () => useViewModeStore.getState();

beforeEach(() => {
  useViewModeStore.setState({ viewMode: 'list' });
});

describe('viewModeStore', () => {
  test('defaults to the list view', () => {
    expect(useViewModeStore.getInitialState().viewMode).toBe('list');
  });

  test('setViewMode switches to the column view', () => {
    store().setViewMode('column');
    expect(store().viewMode).toBe('column');
  });

  test('cycleViewMode steps list → grid → column → list', () => {
    store().cycleViewMode();
    expect(store().viewMode).toBe('grid');
    store().cycleViewMode();
    expect(store().viewMode).toBe('column');
    store().cycleViewMode();
    expect(store().viewMode).toBe('list');
  });

  test('nextViewMode and isViewMode', () => {
    expect(nextViewMode('column')).toBe('list');
    expect(isViewMode('column')).toBe(true);
    expect(isViewMode('columns')).toBe(false);
    expect(isViewMode(undefined)).toBe(false);
  });

  test('only the view mode is persisted, and unknown stored values fall back to list', () => {
    expect(options.name).toBe('file-view-mode');
    expect(options.partialize?.({ ...store(), viewMode: 'column' })).toEqual({
      viewMode: 'column',
    });
    const current = store();
    expect(options.merge?.({ viewMode: 'column' }, current).viewMode).toBe('column');
    expect(options.merge?.({ viewMode: 'tiles' }, current).viewMode).toBe('list');
    expect(options.merge?.(undefined, current).viewMode).toBe('list');
  });
});
