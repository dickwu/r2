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
  toMountRecovery,
  recoveryMatchesTarget,
  canResumeRecovery,
  resolveRecoveryTarget,
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
    recoveries: [],
    recoveryError: null,
    isLoadingRecoveries: false,
    selectedRecoveryId: null,
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
      health: 'mounted',
      healthError: null,
      pendingUploads: 0,
    });
  });

  test('preserves degraded health and pending writes from the backend', () => {
    const info = toMountInfo(
      payload({ health: 'degraded', health_error: 'Upload unavailable', pending_uploads: 3 })
    );
    expect(info.health).toBe('degraded');
    expect(info.healthError).toBe('Upload unavailable');
    expect(info.pendingUploads).toBe(3);
  });
});

const RECOVERY_PAYLOAD = {
  recovery_id: 'saved-1',
  provider: 'r2' as const,
  account_id: 'acc-1',
  bucket: 'photos',
  namespace_id: 'namespace-1',
  path: '/staging/saved-1',
  active: false,
  files: [
    {
      key: 'photo.jpg',
      path: '/staging/saved-1/1',
      size: 128,
      generation: 2,
      state: 'dirty',
      error: null,
    },
  ],
};
const RECOVERY_TARGET = {
  provider: 'r2' as const,
  accountId: 'acc-1',
  accountLabel: 'Photos',
  bucket: 'photos',
  accessKeyId: 'ak',
  secretAccessKey: 'sk',
};

describe('mount recovery', () => {
  test('maps recovery identity and preserves invalid entries for export', () => {
    const recovery = toMountRecovery(RECOVERY_PAYLOAD);
    expect(recovery.recoveryId).toBe('saved-1');
    expect(recovery.accountId).toBe('acc-1');
    expect(recovery.namespaceId).toBe('namespace-1');
    expect(recovery.active).toBe(false);
    const invalid = toMountRecovery({
      recovery_id: 'legacy',
      path: '/legacy',
      files: [],
      active: false,
      error: 'Missing journal',
    });
    expect(invalid.error).toBe('Missing journal');
    expect(canResumeRecovery(invalid)).toBe(false);
  });

  test('resume requires an inactive valid recovery for the same provider, account and bucket', () => {
    const recovery = toMountRecovery(RECOVERY_PAYLOAD);
    expect(recoveryMatchesTarget(recovery, RECOVERY_TARGET)).toBe(true);
    expect(recoveryMatchesTarget(recovery, { ...RECOVERY_TARGET, accountId: 'other' })).toBe(false);
    expect(recoveryMatchesTarget({ ...recovery, provider: 'aws' }, RECOVERY_TARGET)).toBe(false);
    expect(recoveryMatchesTarget({ ...recovery, active: true }, RECOVERY_TARGET)).toBe(false);
    expect(recoveryMatchesTarget({ ...recovery, error: 'Corrupt metadata' }, RECOVERY_TARGET)).toBe(
      false
    );
  });

  test('opening another bucket clears the selected recovery', () => {
    useMountStore.setState({ recoveries: [toMountRecovery(RECOVERY_PAYLOAD)] });
    useMountStore.getState().openMountModal(RECOVERY_TARGET, 'saved-1');
    expect(useMountStore.getState().selectedRecoveryId).toBe('saved-1');
    useMountStore.getState().openMountModal({ ...RECOVERY_TARGET, bucket: 'other' });
    expect(useMountStore.getState().selectedRecoveryId).toBeNull();
    useMountStore.getState().setRecoverySelection('saved-1');
    expect(useMountStore.getState().selectedRecoveryId).toBeNull();
  });

  test('failed refresh preserves recovery rows and exposes the failure', async () => {
    useMountStore.setState({ recoveries: [toMountRecovery(RECOVERY_PAYLOAD)] });
    handleInvoke = async () => {
      throw 'Cannot read saved writes';
    };
    await useMountStore.getState().refreshRecoveries();
    expect(useMountStore.getState().recoveries).toHaveLength(1);
    expect(useMountStore.getState().recoveryError).toBe('Cannot read saved writes');
    expect(useMountStore.getState().isLoadingRecoveries).toBe(false);
  });

  test('refresh maps rows and clears a recovery selection that became active', async () => {
    useMountStore.setState({ recoveries: [toMountRecovery(RECOVERY_PAYLOAD)] });
    useMountStore.getState().openMountModal(RECOVERY_TARGET, 'saved-1');
    handleInvoke = async () => [{ ...RECOVERY_PAYLOAD, active: true }];
    await useMountStore.getState().refreshRecoveries();
    expect(useMountStore.getState().recoveries[0].active).toBe(true);
    expect(useMountStore.getState().selectedRecoveryId).toBeNull();
  });

  test('exports legacy data without deleting it; discard only removes after backend success', async () => {
    const recovery = toMountRecovery({ ...RECOVERY_PAYLOAD, error: 'Legacy journal' });
    useMountStore.setState({ recoveries: [recovery] });
    const calls: [string, InvokeArgs][] = [];
    handleInvoke = async (cmd, args) => {
      calls.push([cmd, args]);
      return '/export/saved-1';
    };
    expect(await useMountStore.getState().exportRecovery('saved-1', '/export')).toBe(
      '/export/saved-1'
    );
    expect(calls[0]).toEqual([
      'export_mount_recovery',
      { recoveryId: 'saved-1', destination: '/export' },
    ]);
    expect(useMountStore.getState().recoveries).toHaveLength(1);
    expect(await useMountStore.getState().discardRecovery('saved-1')).toBe(false);
    useMountStore.setState({ recoveries: [toMountRecovery(RECOVERY_PAYLOAD)] });
    handleInvoke = async () => {
      throw 'Disk unavailable';
    };
    expect(await useMountStore.getState().discardRecovery('saved-1')).toBe(false);
    expect(useMountStore.getState().recoveries).toHaveLength(1);
    handleInvoke = async () => undefined;
    expect(await useMountStore.getState().discardRecovery('saved-1')).toBe(true);
    expect(useMountStore.getState().recoveries).toHaveLength(0);
  });

  test('never exports or discards an active recovery', async () => {
    useMountStore.setState({
      recoveries: [toMountRecovery({ ...RECOVERY_PAYLOAD, active: true })],
    });
    let calls = 0;
    handleInvoke = async () => {
      calls++;
    };
    expect(await useMountStore.getState().exportRecovery('saved-1', '/export')).toBeNull();
    expect(await useMountStore.getState().discardRecovery('saved-1')).toBe(false);
    expect(calls).toBe(0);
  });

  test('legacy folders cannot resume or discard even when identity fields are present', async () => {
    const recovery = toMountRecovery({ ...RECOVERY_PAYLOAD, recovery_id: 'legacy:saved-1' });
    useMountStore.setState({ recoveries: [recovery] });
    expect(canResumeRecovery(recovery)).toBe(false);
    expect(await useMountStore.getState().discardRecovery(recovery.recoveryId)).toBe(false);
  });

  test('resuming sends the recovery id and marks saved writes active only after success', async () => {
    useMountStore.setState({ recoveries: [toMountRecovery(RECOVERY_PAYLOAD)] });
    useMountStore.getState().openMountModal(RECOVERY_TARGET, 'saved-1');
    const input = { ...MOUNT_INPUT, read_only: false, recovery_id: 'saved-1' };
    let sent: InvokeArgs;
    handleInvoke = async (cmd, args) => {
      sent = args;
      return payload();
    };
    expect((await useMountStore.getState().mount(input))?.mountId).toBe('m-1');
    expect(sent).toEqual({ input });
    expect(useMountStore.getState().recoveries[0].active).toBe(true);
    expect(useMountStore.getState().selectedRecoveryId).toBeNull();
  });

  test('wrong account or read-only resumes never invoke the mount command', async () => {
    useMountStore.setState({ recoveries: [toMountRecovery(RECOVERY_PAYLOAD)] });
    let calls = 0;
    handleInvoke = async () => {
      calls++;
      return payload();
    };
    expect(
      await useMountStore
        .getState()
        .mount({ ...MOUNT_INPUT, account_id: 'other', recovery_id: 'saved-1' })
    ).toBeNull();
    expect(
      await useMountStore
        .getState()
        .mount({ ...MOUNT_INPUT, read_only: true, recovery_id: 'saved-1' })
    ).toBeNull();
    expect(calls).toBe(0);
    expect(useMountStore.getState().recoveries[0].active).toBe(false);
  });

  test('resolves an R2 token only when the saved bucket uniquely identifies it', () => {
    const account = {
      provider: 'r2' as const,
      account: { id: 'acc-1', name: 'Photos', created_at: 0, updated_at: 0 },
      tokens: [
        {
          token: {
            id: 1,
            account_id: 'acc-1',
            name: null,
            api_token: '',
            access_key_id: 'ak',
            secret_access_key: 'sk',
            created_at: 0,
            updated_at: 0,
          },
          buckets: [
            {
              id: 1,
              token_id: 1,
              name: 'photos',
              public_domain: null,
              public_domain_scheme: null,
              is_public: false,
              public_path_prefix: null,
              created_at: 0,
              updated_at: 0,
            },
          ],
        },
      ],
    };
    const recovery = toMountRecovery(RECOVERY_PAYLOAD);
    expect(resolveRecoveryTarget(recovery, [account])).toEqual(RECOVERY_TARGET);
    expect(
      resolveRecoveryTarget(recovery, [
        { ...account, tokens: [account.tokens[0], account.tokens[0]] },
      ])
    ).toBeNull();
    expect(resolveRecoveryTarget(recovery, [])).toBeNull();
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
