import { describe, expect, test, mock, beforeEach } from 'bun:test';

/**
 * Store-level contract for the bucket mount feature. The backend owns the
 * truth (`list_mounts` + the `mount-changed` event); these tests pin the
 * state transitions the UI reads: busy flags, the snake_case → camelCase
 * mapping, error capture, and mount/unmount bookkeeping.
 */

type InvokeArgs = Record<string, unknown> | undefined;
type InvokeFn = (cmd: string, args?: InvokeArgs) => Promise<unknown>;
type EventHandler = (event: { payload: unknown }) => void;

// Reassigned per test; the module mocks close over these bindings by
// reference, so each test installs its own fake backend.
let handleInvoke: InvokeFn = async () => undefined;

// Subscriptions outlive individual tests: setupGlobalMountListeners() guards
// against re-subscribing, so the handler registered by the first call is the
// one every later test drives.
const eventHandlers: Record<string, EventHandler> = {};
let listenCalls = 0;

mock.module('@tauri-apps/api/core', () => ({
  invoke: (cmd: string, args?: InvokeArgs) => handleInvoke(cmd, args),
}));

type ListenFn = (event: string, handler: EventHandler) => Promise<() => void>;

const recordingListen: ListenFn = async (event, handler) => {
  listenCalls += 1;
  eventHandlers[event] = handler;
  return () => {
    delete eventHandlers[event];
  };
};

// Swappable so one test can fail a single registration mid-setup.
let listenImpl: ListenFn = recordingListen;

mock.module('@tauri-apps/api/event', () => ({
  listen: (event: string, handler: EventHandler) => listenImpl(event, handler),
}));

// Import after the mocks are registered so the store binds to the fakes.
const {
  useMountStore,
  setupGlobalMountListeners,
  findMount,
  isBucketMounted,
  toMountInfo,
  defaultMountPath,
  flushErrorKey,
  flushErrorMessage,
  shouldReportFlushError,
  FLUSH_ERROR_QUIET_MS,
  applyTransferEvent,
  transferName,
  pruneDeadMountTransfers,
  TRANSFER_RETAIN_MS,
  MAX_TRANSFER_ROWS,
} = await import('./mountStore');
type MountTransferEvent = import('./mountStore').MountTransferEvent;
type MountTransfer = import('./mountStore').MountTransfer;
const { useToastStore } = await import('./toastStore');

function payload(overrides: Record<string, unknown> = {}) {
  return {
    mount_id: 'm-1',
    provider: 'r2' as const,
    account_id: 'acc-1',
    bucket: 'photos',
    local_path: '/Users/me/CloudMounts/photos',
    port: 51234,
    read_only: true,
    mounted_at: 1_700_000_000,
    ...overrides,
  };
}

const MOUNT_INPUT = {
  provider: 'r2' as const,
  account_id: 'acc-1',
  bucket: 'photos',
  local_path: '/Users/me/CloudMounts/photos',
  access_key_id: 'ak',
  secret_access_key: 'sk',
};

beforeEach(() => {
  handleInvoke = async () => undefined;
  useMountStore.setState({
    mounts: [],
    modalOpen: false,
    target: null,
    isMounting: false,
    isUnmounting: false,
    error: null,
  });
});

describe('payload mapping', () => {
  test('maps every snake_case field onto its camelCase counterpart', () => {
    expect(toMountInfo(payload())).toEqual({
      mountId: 'm-1',
      provider: 'r2',
      accountId: 'acc-1',
      bucket: 'photos',
      localPath: '/Users/me/CloudMounts/photos',
      port: 51234,
      readOnly: true,
      mountedAt: 1_700_000_000,
    });
  });
});

describe('modal state', () => {
  const target = {
    provider: 'aws' as const,
    accountId: 'aws-1',
    accountLabel: 'Prod',
    bucket: 'assets',
    accessKeyId: 'ak',
    secretAccessKey: 'sk',
    region: 'us-east-1',
  };

  test('opening carries the target through and clears a stale error', () => {
    useMountStore.setState({ error: 'previous failure' });
    useMountStore.getState().openMountModal(target);

    const state = useMountStore.getState();
    expect(state.modalOpen).toBe(true);
    expect(state.target).toEqual(target);
    expect(state.error).toBeNull();
  });

  test('closing drops the error but keeps the target for the closing animation', () => {
    useMountStore.getState().openMountModal(target);
    useMountStore.setState({ error: 'boom' });
    useMountStore.getState().closeMountModal();

    expect(useMountStore.getState().modalOpen).toBe(false);
    expect(useMountStore.getState().error).toBeNull();
  });
});

describe('refreshMounts', () => {
  test('replaces the list with what the backend reports', async () => {
    handleInvoke = async (cmd) => (cmd === 'list_mounts' ? [payload()] : undefined);

    await useMountStore.getState().refreshMounts();

    expect(useMountStore.getState().mounts.length).toBe(1);
    expect(useMountStore.getState().mounts[0].localPath).toBe('/Users/me/CloudMounts/photos');
  });

  test('leaves the last known list alone when the backend errors', async () => {
    useMountStore.setState({ mounts: [toMountInfo(payload())] });
    handleInvoke = async () => {
      throw 'backend unavailable';
    };

    await useMountStore.getState().refreshMounts();

    expect(useMountStore.getState().mounts.length).toBe(1);
  });
});

describe('mount', () => {
  test('sends the input under an `input` key and records the returned mount', async () => {
    let sent: InvokeArgs;
    handleInvoke = async (cmd, args) => {
      if (cmd !== 'mount_bucket') return undefined;
      sent = args;
      return payload();
    };

    const info = await useMountStore.getState().mount(MOUNT_INPUT);

    expect((sent as { input: unknown }).input).toEqual(MOUNT_INPUT);
    expect(info?.mountId).toBe('m-1');
    expect(useMountStore.getState().mounts.length).toBe(1);
    expect(useMountStore.getState().isMounting).toBe(false);
    expect(useMountStore.getState().error).toBeNull();
  });

  test('is busy while the backend works and settles afterwards', async () => {
    let release: (value: unknown) => void = () => {};
    handleInvoke = () =>
      new Promise((resolve) => {
        release = resolve;
      });

    const pending = useMountStore.getState().mount(MOUNT_INPUT);
    expect(useMountStore.getState().isMounting).toBe(true);

    release(payload());
    await pending;
    expect(useMountStore.getState().isMounting).toBe(false);
  });

  test('keeps a string error verbatim so the sudo hint survives', async () => {
    const backendError =
      'Mounting needs elevated permissions.\nsudo mount -t nfs -o nolock localhost:/ /mnt/photos';
    handleInvoke = async () => {
      throw backendError;
    };

    const info = await useMountStore.getState().mount(MOUNT_INPUT);

    expect(info).toBeNull();
    expect(useMountStore.getState().error).toBe(backendError);
    expect(useMountStore.getState().isMounting).toBe(false);
    expect(useMountStore.getState().mounts.length).toBe(0);
  });

  test('replaces an existing entry rather than duplicating the same mount id', async () => {
    useMountStore.setState({ mounts: [toMountInfo(payload({ local_path: '/old' }))] });
    handleInvoke = async () => payload({ local_path: '/new' });

    await useMountStore.getState().mount(MOUNT_INPUT);

    expect(useMountStore.getState().mounts.length).toBe(1);
    expect(useMountStore.getState().mounts[0].localPath).toBe('/new');
  });
});

describe('unmount', () => {
  test('drops the mount and reports success', async () => {
    useMountStore.setState({
      mounts: [toMountInfo(payload()), toMountInfo(payload({ mount_id: 'm-2', bucket: 'docs' }))],
    });
    let sent: InvokeArgs;
    handleInvoke = async (cmd, args) => {
      if (cmd === 'unmount_bucket') sent = args;
      return undefined;
    };

    const ok = await useMountStore.getState().unmount('m-1');

    expect(ok).toBe(true);
    expect(sent).toEqual({ mountId: 'm-1' });
    expect(useMountStore.getState().mounts.map((m) => m.mountId)).toEqual(['m-2']);
    expect(useMountStore.getState().isUnmounting).toBe(false);
  });

  test('keeps the mount listed when the backend refuses', async () => {
    useMountStore.setState({ mounts: [toMountInfo(payload())] });
    handleInvoke = async () => {
      throw 'Device busy';
    };

    const ok = await useMountStore.getState().unmount('m-1');

    expect(ok).toBe(false);
    expect(useMountStore.getState().mounts.length).toBe(1);
    expect(useMountStore.getState().error).toBe('Device busy');
    expect(useMountStore.getState().isUnmounting).toBe(false);
  });
});

describe('listener registration failure', () => {
  /**
   * Must be the first test that calls setup: the module refuses to subscribe
   * twice, so only the first call reaches the registration path at all.
   */
  test('rolls back a partial registration so a retry starts from zero', async () => {
    listenImpl = async (event, handler) => {
      if (event === 'mount-flush-error') {
        listenCalls += 1;
        throw new Error('listen failed');
      }
      return recordingListen(event, handler);
    };

    try {
      await setupGlobalMountListeners();
    } finally {
      listenImpl = recordingListen;
    }

    // `mount-changed` registered before the failure — it must not survive, or a
    // retry would leave two subscriptions delivering every event twice. The
    // next describe is that retry: it subscribes and loads from scratch.
    expect(eventHandlers['mount-changed']).toBeUndefined();
    expect(eventHandlers['mount-flush-error']).toBeUndefined();
  });
});

describe('mount-changed subscription', () => {
  test('setup loads the current list and then follows the event', async () => {
    handleInvoke = async (cmd) => (cmd === 'list_mounts' ? [payload()] : undefined);

    await setupGlobalMountListeners();

    expect(useMountStore.getState().mounts.length).toBe(1);
    expect(typeof eventHandlers['mount-changed']).toBe('function');

    eventHandlers['mount-changed']({
      payload: { mounts: [payload({ mount_id: 'm-9', bucket: 'docs' })] },
    });

    const mounts = useMountStore.getState().mounts;
    expect(mounts.length).toBe(1);
    expect(mounts[0].mountId).toBe('m-9');
    expect(mounts[0].bucket).toBe('docs');
  });

  test('an empty event clears every mount', async () => {
    await setupGlobalMountListeners();
    useMountStore.setState({ mounts: [toMountInfo(payload())] });

    eventHandlers['mount-changed']({ payload: { mounts: [] } });

    expect(useMountStore.getState().mounts.length).toBe(0);
  });

  test('calling setup again does not subscribe a second time', async () => {
    await setupGlobalMountListeners();
    const before = listenCalls;
    await setupGlobalMountListeners();

    // A delta, not an absolute: earlier tests in this file also call setup.
    expect(listenCalls).toBe(before);
    expect(typeof eventHandlers['mount-changed']).toBe('function');
    expect(typeof eventHandlers['mount-flush-error']).toBe('function');
  });
});

describe('flush-error reporting', () => {
  const event = {
    mount_id: 'm-1',
    bucket: 'photos',
    key: 'trips/iceland.raw',
    error: 'connection reset',
  };

  test('names the file, the bucket and the reason', () => {
    expect(flushErrorMessage(event)).toBe(
      'Upload of "trips/iceland.raw" to photos failed: connection reset'
    );
  });

  test('the same key in two mounts is two different files', () => {
    expect(flushErrorKey(event)).not.toBe(flushErrorKey({ ...event, mount_id: 'm-2' }));
    expect(flushErrorKey(event)).toBe(flushErrorKey({ ...event, error: 'timed out' }));
  });

  describe('shouldReportFlushError', () => {
    test('reports a file once, then stays quiet until the window passes', () => {
      const reported = new Map<string, number>();

      expect(shouldReportFlushError(reported, 'a', 0)).toBe(true);
      expect(shouldReportFlushError(reported, 'a', 1_000)).toBe(false);
      expect(shouldReportFlushError(reported, 'a', FLUSH_ERROR_QUIET_MS - 1)).toBe(false);
      expect(shouldReportFlushError(reported, 'a', FLUSH_ERROR_QUIET_MS)).toBe(true);
    });

    test('keeps a separate window per file', () => {
      const reported = new Map<string, number>();

      expect(shouldReportFlushError(reported, 'a', 0)).toBe(true);
      expect(shouldReportFlushError(reported, 'b', 0)).toBe(true);
      expect(shouldReportFlushError(reported, 'b', 100)).toBe(false);
    });

    test('forgets files that have gone quiet instead of growing forever', () => {
      const reported = new Map<string, number>();
      shouldReportFlushError(reported, 'a', 0);
      shouldReportFlushError(reported, 'b', 0);

      shouldReportFlushError(reported, 'c', FLUSH_ERROR_QUIET_MS * 2);

      expect([...reported.keys()]).toEqual(['c']);
    });
  });

  test('a flush-error event raises one toast, and a repeat within the window raises none', async () => {
    await setupGlobalMountListeners();
    useToastStore.setState({ toasts: [] });

    expect(typeof eventHandlers['mount-flush-error']).toBe('function');
    eventHandlers['mount-flush-error']({ payload: event });
    eventHandlers['mount-flush-error']({ payload: event });

    const toasts = useToastStore.getState().toasts;
    expect(toasts.length).toBe(1);
    expect(toasts[0].kind).toBe('error');
    expect(toasts[0].text).toBe(flushErrorMessage(event));
  });
});

describe('selectors', () => {
  const mounts = [
    toMountInfo(payload()),
    toMountInfo(
      payload({ mount_id: 'm-2', provider: 'aws', account_id: 'aws-1', bucket: 'photos' })
    ),
  ];

  test('findMount keys on provider, account and bucket together', () => {
    expect(findMount(mounts, 'r2', 'acc-1', 'photos')?.mountId).toBe('m-1');
    expect(findMount(mounts, 'aws', 'aws-1', 'photos')?.mountId).toBe('m-2');
    // Same bucket name under a different account is a different bucket.
    expect(findMount(mounts, 'r2', 'acc-2', 'photos')).toBeUndefined();
    expect(findMount(mounts, 'r2', 'acc-1', 'docs')).toBeUndefined();
  });

  test('isBucketMounted answers the sidebar marker question', () => {
    expect(isBucketMounted(mounts, 'r2', 'acc-1', 'photos')).toBe(true);
    expect(isBucketMounted(mounts, 'minio', 'acc-1', 'photos')).toBe(false);
    expect(isBucketMounted([], 'r2', 'acc-1', 'photos')).toBe(false);
  });
});

describe('defaultMountPath', () => {
  test('asks the backend for the suggested path', async () => {
    let sent: InvokeArgs;
    handleInvoke = async (cmd, args) => {
      if (cmd !== 'default_mount_path') return undefined;
      sent = args;
      return '/Users/me/CloudMounts/photos';
    };

    expect(await defaultMountPath('photos')).toBe('/Users/me/CloudMounts/photos');
    expect(sent).toEqual({ bucket: 'photos' });
  });
});

describe('transfer progress', () => {
  function transferEvent(overrides: Partial<MountTransferEvent> = {}): MountTransferEvent {
    return {
      mount_id: 'm-1',
      bucket: 'photos',
      transfer_id: 'm-1:42:up',
      key: 'trips/2024/beach.jpg',
      kind: 'upload',
      state: 'active',
      bytes_done: 10,
      bytes_total: 100,
      speed: 5,
      ...overrides,
    };
  }

  test('a transfer is named after the last segment of its key', () => {
    expect(transferName('trips/2024/beach.jpg')).toBe('beach.jpg');
    expect(transferName('beach.jpg')).toBe('beach.jpg');
    expect(transferName('')).toBe('');
  });

  test('events upsert one row per transfer id, in place', () => {
    const first = applyTransferEvent([], transferEvent({ state: 'waiting', bytes_done: 0 }), 1_000);
    expect(first.length).toBe(1);
    expect(first[0].state).toBe('waiting');
    expect(first[0].name).toBe('beach.jpg');

    const second = applyTransferEvent(
      first,
      transferEvent({ transfer_id: 'm-1:43:up', key: 'other.txt' }),
      2_000
    );
    const third = applyTransferEvent(
      second,
      transferEvent({ bytes_done: 50, state: 'active' }),
      3_000
    );

    expect(third.length).toBe(2);
    // The updated row keeps its position so the dock does not reshuffle.
    expect(third[0].id).toBe('m-1:42:up');
    expect(third[0].bytesDone).toBe(50);
    expect(third[0].state).toBe('active');
    expect(third[1].id).toBe('m-1:43:up');
    // Immutability: the input array still holds the pre-update row.
    expect(second[0].bytesDone).toBe(0);
    expect(second[0].state).toBe('waiting');
  });

  test('a removed transfer disappears rather than reading as done', () => {
    const one = applyTransferEvent([], transferEvent({ state: 'waiting' }), 1_000);
    const gone = applyTransferEvent(one, transferEvent({ state: 'removed' }), 2_000);
    expect(gone.length).toBe(0);
  });

  test('finished rows are pruned once they have lingered past the retain window', () => {
    const done = applyTransferEvent([], transferEvent({ state: 'done', bytes_done: 100 }), 1_000);
    // Still shown while fresh…
    const kept = applyTransferEvent(
      done,
      transferEvent({ transfer_id: 'm-1:43:up' }),
      1_000 + TRANSFER_RETAIN_MS - 1
    );
    expect(kept.map((t: MountTransfer) => t.id)).toContain('m-1:42:up');
    // …and dropped when stale, while live rows always survive.
    const pruned = applyTransferEvent(
      kept,
      transferEvent({ transfer_id: 'm-1:44:down', kind: 'download' }),
      1_000 + TRANSFER_RETAIN_MS + 1
    );
    expect(pruned.map((t: MountTransfer) => t.id)).not.toContain('m-1:42:up');
    expect(pruned.map((t: MountTransfer) => t.id)).toContain('m-1:43:up');
  });

  test('the row count is capped, evicting finished then queued rows first', () => {
    let transfers: MountTransfer[] = [];
    // One old finished row, one old queued row, then a flood of queued rows.
    transfers = applyTransferEvent(
      transfers,
      transferEvent({ transfer_id: 'old-done', state: 'done' }),
      1_000
    );
    transfers = applyTransferEvent(
      transfers,
      transferEvent({ transfer_id: 'old-waiting', state: 'waiting' }),
      1_001
    );
    for (let i = 0; i < MAX_TRANSFER_ROWS; i += 1) {
      transfers = applyTransferEvent(
        transfers,
        transferEvent({ transfer_id: `m-1:${i}:up`, state: 'waiting' }),
        2_000 + i
      );
    }

    expect(transfers.length).toBe(MAX_TRANSFER_ROWS);
    const ids = transfers.map((t: MountTransfer) => t.id);
    // The finished row went first, then the oldest queued row.
    expect(ids).not.toContain('old-done');
    expect(ids).not.toContain('old-waiting');
    expect(ids).toContain(`m-1:${MAX_TRANSFER_ROWS - 1}:up`);
  });

  test('active rows survive the cap', () => {
    let transfers: MountTransfer[] = [];
    transfers = applyTransferEvent(
      transfers,
      transferEvent({ transfer_id: 'busy', state: 'active' }),
      1_000
    );
    for (let i = 0; i < MAX_TRANSFER_ROWS + 5; i += 1) {
      transfers = applyTransferEvent(
        transfers,
        transferEvent({ transfer_id: `w-${i}`, state: 'waiting' }),
        2_000 + i
      );
    }
    expect(transfers.map((t: MountTransfer) => t.id)).toContain('busy');
    expect(transfers.length).toBe(MAX_TRANSFER_ROWS);
  });

  test('live rows of a vanished mount are pruned, finished rows kept to age out', () => {
    const transfers = [
      applyTransferEvent([], transferEvent({ transfer_id: 'live', state: 'waiting' }), 1_000)[0],
      applyTransferEvent([], transferEvent({ transfer_id: 'ended', state: 'done' }), 1_000)[0],
    ];

    // The mount list no longer contains m-1 (unmounted).
    const pruned = pruneDeadMountTransfers(transfers, []);
    const ids = pruned.map((t: MountTransfer) => t.id);
    expect(ids).not.toContain('live');
    expect(ids).toContain('ended');

    // With the mount still present nothing is pruned.
    const kept = pruneDeadMountTransfers(transfers, [toMountInfo(payload())]);
    expect(kept.length).toBe(2);
  });

  test('the store folds mount-transfer events through the reducer', async () => {
    await setupGlobalMountListeners();
    const handler = eventHandlers['mount-transfer'];
    expect(typeof handler).toBe('function');

    handler({ payload: transferEvent({ state: 'waiting', bytes_done: 0 }) });
    expect(useMountStore.getState().transfers.length).toBe(1);

    handler({ payload: transferEvent({ state: 'done', bytes_done: 100 }) });
    expect(useMountStore.getState().transfers[0].state).toBe('done');

    useMountStore.getState().clearFinishedTransfers();
    expect(useMountStore.getState().transfers.length).toBe(0);
  });
});
