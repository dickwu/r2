import { beforeEach, describe, expect, test } from 'bun:test';
import { useAccountStore, type CurrentConfig } from './accountStore';
import { useDownloadStore, type DownloadSession } from './downloadStore';
import { useTransferErrorStore } from './transferErrorStore';

const TASK_ID = 'download-1';

const session = (overrides: Partial<DownloadSession> = {}): DownloadSession => ({
  id: TASK_ID,
  object_key: 'photos/2026/IMG_2214.CR2',
  file_name: 'IMG_2214.CR2',
  file_size: 1000,
  downloaded_bytes: 400,
  local_path: '/Users/me/Downloads',
  bucket: 'media',
  account_id: 'acct',
  status: 'downloading',
  error: null,
  created_at: 1,
  updated_at: 1,
  ...overrides,
});

const browsing: CurrentConfig = {
  provider: 'r2',
  account_id: 'acct',
  account_name: 'Greenwoods R2',
  access_key_id: 'key',
  secret_access_key: 'secret',
  bucket: 'media',
  public_domain: null,
};

// The backend reports a failed download as 'failed'; the store maps it to 'error'.
const statusChanged = (status: string, error: string | null = null) =>
  useDownloadStore.getState().handleStatusChanged({ task_id: TASK_ID, status, error });

const failures = () => useTransferErrorStore.getState().failures;
const task = () => useDownloadStore.getState().tasks.find((t) => t.id === TASK_ID);

/** How many times `run` reported to the failure store: every report() is one store update. */
const countReports = (run: () => void): number => {
  let reports = 0;
  const unsubscribe = useTransferErrorStore.subscribe(() => {
    reports += 1;
  });
  try {
    run();
  } finally {
    unsubscribe();
  }
  return reports;
};

beforeEach(() => {
  useDownloadStore.getState().clearAll();
  useTransferErrorStore.getState().clear();
  useAccountStore.setState({ currentConfig: null });
});

describe('download failures reach the failure modal on the way into error only', () => {
  test('a download entering failed reports its record once and opens the modal', () => {
    useDownloadStore.getState().loadFromDatabase([session()]);
    expect(countReports(() => statusChanged('failed', 'AccessDenied: Access Denied'))).toBe(1);

    expect(task()?.status).toBe('error');
    expect(useTransferErrorStore.getState().isOpen).toBe(true);
    const [failure] = failures();
    expect(failures()).toHaveLength(1);
    expect(failure).toEqual({
      id: `download:${TASK_ID}`,
      kind: 'download',
      name: 'IMG_2214.CR2',
      message: 'AccessDenied: Access Denied',
      occurredAt: failure.occurredAt,
      key: 'photos/2026/IMG_2214.CR2',
      bucket: 'media',
      progress: { done: 400, total: 1000 },
    });
  });

  test('the same failure event again does not report again', () => {
    useDownloadStore.getState().loadFromDatabase([session()]);
    statusChanged('failed', 'AccessDenied: Access Denied');
    useTransferErrorStore.getState().clear();

    expect(countReports(() => statusChanged('failed', 'AccessDenied: Access Denied'))).toBe(0);
    expect(failures()).toHaveLength(0);
  });

  test('a new message on a download that already failed updates the row without reporting', () => {
    useDownloadStore.getState().loadFromDatabase([session()]);
    statusChanged('failed', 'AccessDenied: Access Denied');
    useTransferErrorStore.getState().clear();

    expect(countReports(() => statusChanged('failed', 'SlowDown: Reduce your rate.'))).toBe(0);
    expect(task()?.error).toBe('SlowDown: Reduce your rate.');
    expect(failures()).toHaveLength(0);
  });

  test('a retried download that fails again reports again', () => {
    useDownloadStore.getState().loadFromDatabase([session()]);
    statusChanged('failed', 'AccessDenied: Access Denied');
    useTransferErrorStore.getState().clear();

    const reports = countReports(() => {
      statusChanged('pending');
      statusChanged('downloading');
      statusChanged('failed', 'AccessDenied: Access Denied');
    });
    expect(reports).toBe(1);
    expect(failures()).toHaveLength(1);
  });

  test('a failure restored from the database never reports, then or on its next event', () => {
    const restored = session({ status: 'failed', error: 'AccessDenied: Access Denied' });
    expect(countReports(() => useDownloadStore.getState().loadFromDatabase([restored]))).toBe(0);
    expect(task()?.status).toBe('error');

    expect(countReports(() => statusChanged('failed', 'AccessDenied: Access Denied'))).toBe(0);
    expect(useTransferErrorStore.getState().isOpen).toBe(false);
  });

  test('a failure without a message still reports, as "Download failed"', () => {
    useDownloadStore.getState().loadFromDatabase([session()]);
    statusChanged('failed');
    expect(failures()[0]?.message).toBe('Download failed');
  });

  test('an event for a download the store does not hold reports nothing', () => {
    expect(countReports(() => statusChanged('failed', 'AccessDenied: Access Denied'))).toBe(0);
  });
});

describe('download tasks carry their bucket', () => {
  test('a queued download is stamped with the browsed bucket', () => {
    useAccountStore.setState({ currentConfig: browsing });
    useDownloadStore.getState().addTask({
      id: TASK_ID,
      key: 'photos/2026/IMG_2214.CR2',
      fileName: 'IMG_2214.CR2',
      fileSize: 1000,
      localPath: '/Users/me/Downloads',
    });
    expect(task()?.bucket).toBe('media');

    statusChanged('downloading');
    statusChanged('failed', 'AccessDenied: Access Denied');
    expect(failures()[0]?.bucket).toBe('media');
  });

  test('a batch queued with nothing browsed leaves the bucket unknown', () => {
    useDownloadStore.getState().addTasks([
      {
        id: TASK_ID,
        key: 'a.txt',
        fileName: 'a.txt',
        fileSize: 1,
        localPath: '/Users/me/Downloads',
      },
    ]);
    expect(task()?.bucket).toBeUndefined();
  });

  test('a download restored from the database keeps its session bucket', () => {
    useDownloadStore.getState().loadFromDatabase([session({ bucket: 'archive' })]);
    expect(task()?.bucket).toBe('archive');
  });
});

describe('reloads and retries', () => {
  test('a reload that lands before the status event reports the failure once', () => {
    useDownloadStore.getState().loadFromDatabase([session()]);
    const reports = countReports(() => {
      useDownloadStore
        .getState()
        .loadFromDatabase([session({ status: 'failed', error: 'AccessDenied: Access Denied' })]);
      statusChanged('failed', 'AccessDenied: Access Denied');
    });
    expect(reports).toBe(1);
    expect(failures()).toHaveLength(1);
    expect(failures()[0].id).toBe(`download:${TASK_ID}`);
    expect(useTransferErrorStore.getState().isOpen).toBe(true);
  });

  test('a startup restore of a failed download reports nothing', () => {
    const reports = countReports(() =>
      useDownloadStore
        .getState()
        .loadFromDatabase([session({ status: 'failed', error: 'AccessDenied: Access Denied' })])
    );
    expect(reports).toBe(0);
    expect(failures()).toHaveLength(0);
  });

  test('a retry ends the record, so failing again opens the modal again', () => {
    useDownloadStore.getState().loadFromDatabase([session()]);
    statusChanged('failed', 'AccessDenied: Access Denied');
    useTransferErrorStore.getState().close();

    statusChanged('downloading');
    expect(failures()).toHaveLength(0);

    statusChanged('failed', 'AccessDenied: Access Denied');
    expect(failures()).toHaveLength(1);
    expect(useTransferErrorStore.getState().isOpen).toBe(true);
  });

  test('a stale reload that still shows the download running does not erase a fresh record', () => {
    useDownloadStore.getState().loadFromDatabase([session()]);
    statusChanged('failed', 'AccessDenied: Access Denied');
    useDownloadStore.getState().loadFromDatabase([session({ status: 'downloading' })]);
    expect(failures()).toHaveLength(1);
    expect(useTransferErrorStore.getState().isOpen).toBe(true);
  });
});
