import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { selectHasActiveUploads, useUploadStore, type UploadTask } from './uploadStore';

type Settle = (outcome: Partial<UploadTask>) => void;

// A runner whose uploads finish only when the test settles them
function controlledRunner() {
  const running = new Map<string, Settle>();
  const cancelled: string[] = [];
  const run = (task: UploadTask) =>
    new Promise<void>((resolve) => {
      running.set(task.id, (outcome) => {
        running.delete(task.id);
        useUploadStore.getState().updateTask(task.id, outcome);
        resolve();
      });
    });
  const cancel = (id: string) => {
    cancelled.push(id);
    running.get(id)?.({ status: 'cancelled', speed: 0 });
  };
  return { running, cancelled, run, cancel };
}

const flush = async () => {
  for (let i = 0; i < 5; i++) await Promise.resolve();
};

const ids = () => useUploadStore.getState().tasks.map((t) => t.id);
const statusOf = (id: string) => useUploadStore.getState().tasks.find((t) => t.id === id)?.status;
const statuses = () => useUploadStore.getState().tasks.map((t) => t.status);

function addFiles(count: number) {
  useUploadStore.getState().addTasks(
    Array.from({ length: count }, (_, i) => ({
      filePath: `/tmp/file-${i}.bin`,
      fileName: `file-${i}.bin`,
      fileSize: 1024,
      contentType: 'application/octet-stream',
    }))
  );
}

describe('upload queue', () => {
  let runner: ReturnType<typeof controlledRunner>;

  beforeEach(() => {
    useUploadStore.setState({ tasks: [], maxConcurrent: 3 });
    runner = controlledRunner();
    useUploadStore.getState().setUploadRunner(runner.run, runner.cancel);
  });

  afterEach(() => {
    useUploadStore.getState().setUploadRunner(null);
  });

  test('starts at most 3 uploads and queues the rest', async () => {
    addFiles(10);
    useUploadStore.getState().startAllPending();
    await flush();

    expect(statuses().filter((s) => s === 'uploading')).toHaveLength(3);
    expect(statuses().filter((s) => s === 'queued')).toHaveLength(7);
    expect(runner.running.size).toBe(3);
    expect(selectHasActiveUploads(useUploadStore.getState())).toBe(true);
  });

  test('starts the next queued upload when one finishes, in order', async () => {
    addFiles(5);
    const [a, b, c, d, e] = ids();
    useUploadStore.getState().startAllPending();
    await flush();

    runner.running.get(b)!({ status: 'success', progress: 100 });
    await flush();
    expect(statusOf(d)).toBe('uploading');
    expect(statusOf(e)).toBe('queued');

    runner.running.get(a)!({ status: 'error', error: 'AccessDenied' });
    await flush();
    expect(statusOf(e)).toBe('uploading');
    expect([...runner.running.keys()].sort()).toEqual([c, d, e].sort());
  });

  test('every upload eventually runs with the cap respected throughout', async () => {
    addFiles(8);
    useUploadStore.getState().startAllPending();
    await flush();
    let peak = 0;
    while (runner.running.size > 0) {
      peak = Math.max(peak, runner.running.size);
      const [first] = runner.running.keys();
      runner.running.get(first)!({ status: 'success' });
      await flush();
    }
    expect(peak).toBe(3);
    expect(statuses().every((s) => s === 'success')).toBe(true);
    expect(selectHasActiveUploads(useUploadStore.getState())).toBe(false);
  });

  test('raising the limit starts more queued uploads right away', async () => {
    addFiles(6);
    useUploadStore.getState().startAllPending();
    await flush();
    useUploadStore.getState().setMaxConcurrent(5);
    await flush();
    expect(runner.running.size).toBe(5);
    useUploadStore.getState().setMaxConcurrent(99);
    expect(useUploadStore.getState().maxConcurrent).toBe(5);
  });

  test('cancelling a queued upload never starts it; cancelling a running one asks the backend', async () => {
    addFiles(4);
    const [a, , , d] = ids();
    useUploadStore.getState().startAllPending();
    await flush();

    useUploadStore.getState().cancelTask(d);
    expect(statusOf(d)).toBe('cancelled');

    useUploadStore.getState().cancelTask(a);
    await flush();
    expect(runner.cancelled).toEqual([a]);
    expect(statusOf(a)).toBe('cancelled');
    // The slot freed by the cancel is not given to the cancelled queued task
    expect(statusOf(d)).toBe('cancelled');
    expect(runner.running.size).toBe(2);
  });

  test('removing a running upload cancels it and frees its slot', async () => {
    addFiles(4);
    const [a, , , d] = ids();
    useUploadStore.getState().startAllPending();
    await flush();

    useUploadStore.getState().removeTask(a);
    await flush();
    expect(runner.cancelled).toEqual([a]);
    expect(ids()).not.toContain(a);
    expect(statusOf(d)).toBe('uploading');
  });

  test('a runner that settles without an outcome fails the task instead of holding the slot', async () => {
    useUploadStore.getState().setUploadRunner(async () => {}, runner.cancel);
    addFiles(1);
    const [a] = ids();
    useUploadStore.getState().startAllPending();
    await flush();
    expect(statusOf(a)).toBe('error');
  });

  test('without a runner, started uploads wait queued until one is installed', async () => {
    useUploadStore.getState().setUploadRunner(null);
    addFiles(2);
    useUploadStore.getState().startAllPending();
    await flush();
    expect(statuses()).toEqual(['queued', 'queued']);

    useUploadStore.getState().setUploadRunner(runner.run, runner.cancel);
    await flush();
    expect(statuses()).toEqual(['uploading', 'uploading']);
  });

  test('clearFinished keeps queued and running uploads', async () => {
    addFiles(5);
    const [a] = ids();
    useUploadStore.getState().startAllPending();
    await flush();
    runner.running.get(a)!({ status: 'success' });
    await flush();
    useUploadStore.getState().clearFinished();
    expect(ids()).not.toContain(a);
    expect(statuses().sort()).toEqual(['queued', 'uploading', 'uploading', 'uploading']);
  });
});
