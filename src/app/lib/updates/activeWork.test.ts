import { describe, expect, test } from 'bun:test';
import { countActiveTransfers, describeActiveWork, hasActiveWork } from './activeWork';

describe('activeWork', () => {
  test('counts only transfers still running or waiting', () => {
    expect(
      countActiveTransfers(
        [{ status: 'uploading' }, { status: 'queued' }, { status: 'success' }, { status: 'error' }],
        [{ status: 'pending' }, { status: 'completed' }],
        [{ status: 'finishing' }, { status: 'failed' }]
      )
    ).toBe(4);
  });

  test('says what a restart would cost', () => {
    expect(describeActiveWork({ transfers: 0, mountUploads: 0, unsavedEditors: 0 })).toBe('');
    expect(describeActiveWork({ transfers: 1, mountUploads: 0, unsavedEditors: 0 })).toBe(
      'stop 1 transfer'
    );
    expect(describeActiveWork({ transfers: 2, mountUploads: 0, unsavedEditors: 1 })).toBe(
      'stop 2 transfers and lose your unsaved changes in the editor'
    );
    expect(describeActiveWork({ transfers: 2, mountUploads: 3, unsavedEditors: 1 })).toBe(
      'stop 2 transfers, stop 3 uploads from a mounted drive and lose your unsaved changes in the editor'
    );
    expect(hasActiveWork({ transfers: 0, mountUploads: 0, unsavedEditors: 1 })).toBe(true);
  });
});
