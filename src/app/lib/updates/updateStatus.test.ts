import { describe, expect, test } from 'bun:test';
import { describeUpdateError } from './updateErrors';
import { progressAmount, updateStatusView, type UpdateStatusInput } from './updateStatus';

const base: UpdateStatusInput = {
  phase: 'idle',
  manualCheck: false,
  currentVersion: '0.3.17',
  update: null,
  progress: { downloaded: 0, total: null },
  failure: null,
};
const view = (patch: Partial<UpdateStatusInput>) => updateStatusView({ ...base, ...patch });
const update = { version: '0.3.18' };

describe('updateStatusView', () => {
  test('at rest it is the version, and clicking checks', () => {
    expect(view({})).toMatchObject({
      label: 'v0.3.17',
      action: 'check',
      hint: 'Check for updates',
    });
  });

  test('a background check stays still; one you asked for says so', () => {
    expect(view({ phase: 'checking' })).toMatchObject({ label: 'v0.3.17', spinning: false });
    expect(view({ phase: 'checking', manualCheck: true })).toMatchObject({
      label: 'Checking for updates…',
      action: 'none',
      spinning: true,
    });
    expect(view({ phase: 'upToDate' }).label).toBe("You're up to date");
  });

  test('an update is an accent pill that opens the dialog', () => {
    expect(view({ phase: 'available', update })).toMatchObject({
      label: 'v0.3.18 available',
      tone: 'accent',
      action: 'openDialog',
    });
    // A background re-check keeps showing the offer.
    expect(view({ phase: 'checking', update }).label).toBe('v0.3.18 available');
  });

  test('progress in words', () => {
    expect(
      view({ phase: 'downloading', update, progress: { downloaded: 430, total: 1000 } }).label
    ).toBe('Downloading update 43%');
    expect(view({ phase: 'downloading', update }).label).toBe('Downloading update…');
    expect(view({ phase: 'installing', update }).label).toBe('Installing update…');
    expect(view({ phase: 'readyToRestart', update }).label).toBe('Restart to update');
    expect(progressAmount({ downloaded: 1536, total: 3072 })).toBe('1.5 KB of 3 KB');
  });

  test('failures', () => {
    const check = describeUpdateError('check', 'dns error');
    expect(view({ phase: 'error', failure: check })).toMatchObject({
      label: "Couldn't check for updates",
      tone: 'error',
      action: 'retryCheck',
    });
    const download = describeUpdateError('download', 'connection reset');
    expect(view({ phase: 'error', failure: download, update })).toMatchObject({
      label: 'Update failed',
      action: 'openDialog',
    });
  });
});
