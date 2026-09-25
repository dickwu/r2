import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { useToastStore } from '@/app/stores/toastStore';
import { useTransferErrorStore } from '@/app/stores/transferErrorStore';
import type { ProviderAccount } from '@/app/stores/accountStore';
import type { TransferFailure } from '@/app/lib/transferFailure';

// Global listener state - persists across component unmounts
let globalListenersSetup = false;
let globalUnlisteners: UnlistenFn[] = [];

export type MountProvider = 'r2' | 'aws' | 'minio' | 'rustfs';
export type MountHealth = 'mounted' | 'degraded' | 'offline' | 'unmounting';

/** A live mount, as reported by the backend (camelCase mirror of `MountInfoPayload`). */
export interface MountInfo {
  mountId: string;
  provider: MountProvider;
  accountId: string;
  bucket: string;
  localPath: string;
  port: number;
  readOnly: boolean;
  mountedAt: number;
  health: MountHealth;
  healthError: string | null;
  pendingUploads: number;
}

/** Raw `MountInfo` shape on the wire — snake_case, as the Rust structs serialize. */
export interface MountInfoPayload {
  mount_id: string;
  provider: MountProvider;
  account_id: string;
  bucket: string;
  local_path: string;
  port: number;
  read_only: boolean;
  mounted_at: number;
  health?: MountHealth;
  health_error?: string | null;
  pending_uploads?: number;
}

export interface MountRecoveryFile {
  key: string;
  size: number;
  path: string;
  generation: number;
  state: string;
  error?: string | null;
}

export function recoveryStateLabel(state: string): string {
  const labels: Record<string, string> = {
    waiting: 'Waiting to upload',
    uploading: 'Upload interrupted',
    failed: 'Upload failed',
    paused: 'Needs attention',
    replay_pending: 'Restore interrupted write',
    rename_recovery: 'Finish rename',
    namespace_recovery: 'Finish saved change',
    unreadable: 'Needs manual recovery',
  };
  return labels[state] ?? 'Needs attention';
}

export interface MountRecoveryPayload {
  recovery_id: string;
  provider?: MountProvider | null;
  account_id?: string | null;
  bucket?: string | null;
  namespace_id?: string | null;
  path: string;
  files: MountRecoveryFile[];
  error?: string | null;
  active: boolean;
}

export interface MountRecovery {
  recoveryId: string;
  provider: MountProvider | null;
  accountId: string | null;
  bucket: string | null;
  namespaceId: string | null;
  path: string;
  files: MountRecoveryFile[];
  error: string | null;
  active: boolean;
}

/** `mount-changed` payload: the backend always sends the full mount list. */
export interface MountChangedEvent {
  mounts: MountInfoPayload[];
}

/**
 * `mount-flush-error` payload: one file in a writable mount could not be
 * uploaded. Fire-and-forget — the backend keeps retrying and keeps the staged
 * copy on disk either way.
 */
export interface MountFlushErrorEvent {
  mount_id: string;
  bucket: string;
  key: string;
  error: string;
}

/**
 * `mount-transfer` payload: one file's background transfer moved. Uploads are
 * staged writes on their way to the bucket; downloads are objects being staged
 * locally before an in-place edit. `transfer_id` is stable per file per
 * direction, so events update one row instead of appending new ones.
 */
export interface MountTransferEvent {
  mount_id: string;
  bucket: string;
  transfer_id: string;
  key: string;
  kind: 'upload' | 'download';
  state: 'waiting' | 'active' | 'done' | 'error' | 'removed';
  bytes_done: number;
  bytes_total: number;
  speed: number;
  error?: string | null;
}

/** One live (or recently finished) mount transfer, as the dock renders it. */
export interface MountTransfer {
  id: string;
  mountId: string;
  bucket: string;
  key: string;
  /** Last path segment of the key, for display. */
  name: string;
  kind: 'upload' | 'download';
  state: 'waiting' | 'active' | 'done' | 'error';
  bytesDone: number;
  bytesTotal: number;
  speed: number;
  error?: string;
  updatedAt: number;
}

/** Argument to `mount_bucket`. Field names must stay snake_case for serde. */
export interface MountBucketInput {
  provider: MountProvider;
  account_id: string;
  bucket: string;
  local_path: string;
  access_key_id: string;
  secret_access_key: string;
  region?: string | null;
  endpoint_url?: string | null;
  force_path_style?: boolean | null;
  /** Absent or null mounts writable, which is the default. */
  read_only?: boolean | null;
  recovery_id?: string | null;
}

/**
 * Everything needed to mount one bucket, captured when the menu item is
 * clicked. Credentials come straight off the sidebar's account/token data, so
 * any listed bucket can be mounted without first selecting it.
 */
export interface MountTarget {
  provider: MountProvider;
  accountId: string;
  accountLabel: string;
  bucket: string;
  accessKeyId: string;
  secretAccessKey: string;
  region?: string | null;
  endpointUrl?: string | null;
  forcePathStyle?: boolean | null;
}

interface MountStore {
  mounts: MountInfo[];
  transfers: MountTransfer[];
  modalOpen: boolean;
  target: MountTarget | null;
  isMounting: boolean;
  isUnmounting: boolean;
  error: string | null;
  recoveries: MountRecovery[];
  recoveryError: string | null;
  isLoadingRecoveries: boolean;
  selectedRecoveryId: string | null;

  openMountModal: (target: MountTarget, recoveryId?: string) => void;
  closeMountModal: () => void;
  setRecoverySelection: (recoveryId: string | null) => void;
  refreshRecoveries: () => Promise<void>;
  exportRecovery: (recoveryId: string, destination: string) => Promise<string | null>;
  discardRecovery: (recoveryId: string) => Promise<boolean>;
  clearError: () => void;
  setMounts: (mounts: MountInfo[]) => void;
  applyTransfer: (event: MountTransferEvent) => void;
  clearFinishedTransfers: () => void;
  refreshMounts: () => Promise<void>;
  mount: (input: MountBucketInput) => Promise<MountInfo | null>;
  unmount: (mountId: string) => Promise<boolean>;
}

export function toMountInfo(payload: MountInfoPayload): MountInfo {
  return {
    mountId: payload.mount_id,
    provider: payload.provider,
    accountId: payload.account_id,
    bucket: payload.bucket,
    localPath: payload.local_path,
    port: payload.port,
    readOnly: payload.read_only,
    mountedAt: payload.mounted_at,
    health: payload.health ?? 'mounted',
    healthError: payload.health_error ?? null,
    pendingUploads: payload.pending_uploads ?? 0,
  };
}

export function toMountRecovery(payload: MountRecoveryPayload): MountRecovery {
  return {
    recoveryId: payload.recovery_id,
    provider: payload.provider ?? null,
    accountId: payload.account_id ?? null,
    bucket: payload.bucket ?? null,
    namespaceId: payload.namespace_id ?? null,
    path: payload.path,
    files: payload.files,
    error: payload.error ?? null,
    active: payload.active,
  };
}

export function canResumeRecovery(recovery: MountRecovery): boolean {
  return (
    !recovery.recoveryId.startsWith('legacy:') &&
    !recovery.active &&
    !recovery.error &&
    !!recovery.provider &&
    !!recovery.accountId &&
    !!recovery.bucket &&
    !!recovery.namespaceId &&
    recovery.files.some((file) => file.state !== 'unreadable')
  );
}

/** UI filtering only: the backend must also verify the exact storage namespace. */
export function recoveryMatchesTarget(
  recovery: MountRecovery,
  target: Pick<MountTarget, 'provider' | 'accountId' | 'bucket'>
): boolean {
  return (
    canResumeRecovery(recovery) &&
    recovery.provider === target.provider &&
    recovery.accountId === target.accountId &&
    recovery.bucket === target.bucket
  );
}

/** Reuse saved credentials without changing the selected browsing account. */
export function resolveRecoveryTarget(
  recovery: MountRecovery,
  accounts: ProviderAccount[]
): MountTarget | null {
  if (!canResumeRecovery(recovery)) return null;
  const accountData = accounts.find(
    (a) => a.provider === recovery.provider && a.account.id === recovery.accountId
  );
  if (!accountData) return null;
  const common = {
    accountId: accountData.account.id,
    accountLabel: accountData.account.name || accountData.account.id,
    bucket: recovery.bucket!,
  };
  if (accountData.provider === 'r2') {
    const tokens = accountData.tokens.filter((t) =>
      t.buckets.some((b) => b.name === recovery.bucket)
    );
    if (tokens.length !== 1) return null;
    const { token } = tokens[0];
    if (!token.access_key_id || !token.secret_access_key) return null;
    return {
      ...common,
      provider: 'r2',
      accessKeyId: token.access_key_id,
      secretAccessKey: token.secret_access_key,
    };
  }
  const { account } = accountData;
  if (!account.access_key_id || !account.secret_access_key) return null;
  return {
    ...common,
    provider: accountData.provider,
    accessKeyId: account.access_key_id,
    secretAccessKey: account.secret_access_key,
    endpointUrl: account.endpoint_host
      ? `${account.endpoint_scheme || 'https'}://${account.endpoint_host}`
      : null,
    region: accountData.provider === 'aws' ? accountData.account.region : null,
    forcePathStyle: accountData.provider === 'rustfs' ? true : account.force_path_style,
  };
}

// ── Transfer progress ─────────────────────────────────────────────

/** How long a finished or failed transfer row lingers before it is pruned. */
export const TRANSFER_RETAIN_MS = 60_000;

/**
 * Ceiling on transfer rows held (and therefore rendered). A folder copy can
 * queue thousands of files; the dock stays useful by keeping the most recent
 * rows — evicted queued rows reappear the moment their upload starts.
 */
export const MAX_TRANSFER_ROWS = 200;

/** Display name for a transfer: the last path segment of its key. */
export function transferName(key: string): string {
  const segments = key.split('/').filter(Boolean);
  return segments[segments.length - 1] ?? key;
}

/**
 * Folds one `mount-transfer` event into the transfer list, immutably.
 *
 * A `removed` state deletes the row — the file was deleted mid-copy and there
 * is nothing left to report. Everything else upserts by `transfer_id`, keeping
 * the row's position so the dock does not reshuffle mid-upload. Terminal rows
 * that have sat around longer than [`TRANSFER_RETAIN_MS`] are pruned on the
 * way past, so the list cannot grow for as long as a mount lives.
 */
export function applyTransferEvent(
  transfers: MountTransfer[],
  event: MountTransferEvent,
  now: number
): MountTransfer[] {
  const fresh = transfers.filter(
    (t) => (t.state !== 'done' && t.state !== 'error') || now - t.updatedAt < TRANSFER_RETAIN_MS
  );

  if (event.state === 'removed') {
    return fresh.filter((t) => t.id !== event.transfer_id);
  }

  const next: MountTransfer = {
    id: event.transfer_id,
    mountId: event.mount_id,
    bucket: event.bucket,
    key: event.key,
    name: transferName(event.key),
    kind: event.kind,
    state: event.state,
    bytesDone: event.bytes_done,
    bytesTotal: event.bytes_total,
    speed: event.speed,
    error: event.error ?? undefined,
    updatedAt: now,
  };

  const existing = fresh.findIndex((t) => t.id === event.transfer_id);
  if (existing === -1) {
    return capTransfers([...fresh, next]);
  }
  return fresh.map((t, index) => (index === existing ? next : t));
}

/**
 * Keeps the list under [`MAX_TRANSFER_ROWS`], evicting the oldest finished
 * rows first and the oldest queued rows after that. Active rows are never
 * evicted — their count is bounded by the backend's transfer concurrency.
 */
function capTransfers(transfers: MountTransfer[]): MountTransfer[] {
  const overflow = transfers.length - MAX_TRANSFER_ROWS;
  if (overflow <= 0) return transfers;

  const byAge = (a: MountTransfer, b: MountTransfer) => a.updatedAt - b.updatedAt;
  const terminal = transfers.filter((t) => t.state === 'done' || t.state === 'error').sort(byAge);
  const waiting = transfers.filter((t) => t.state === 'waiting').sort(byAge);
  const evicted = new Set([...terminal, ...waiting].slice(0, overflow).map((t) => t.id));
  return transfers.filter((t) => !evicted.has(t.id));
}

/**
 * Drops live rows belonging to mounts that no longer exist. After an unmount
 * the backend never emits for that mount id again, so a `waiting`/`active`
 * row left behind would keep the dock open forever. Finished rows stay — they
 * age out on their own.
 */
export function pruneDeadMountTransfers(
  transfers: MountTransfer[],
  mounts: MountInfo[]
): MountTransfer[] {
  const alive = new Set(mounts.map((m) => m.mountId));
  return transfers.filter((t) => alive.has(t.mountId) || t.state === 'done' || t.state === 'error');
}

/**
 * The failure record for a transfer that failed with a message, or null. The
 * store reports it when the transfer fails and the transfer dock shows it, so
 * both build the identical record.
 */
export function mountFailureRecord(transfer: MountTransfer): TransferFailure | null {
  if (transfer.state !== 'error' || !transfer.error?.trim()) return null;
  return {
    id: `mount:${transfer.id}`,
    kind: 'mount',
    name: transfer.name,
    message: transfer.error,
    occurredAt: transfer.updatedAt,
    key: transfer.key,
    bucket: transfer.bucket,
    progress: { done: transfer.bytesDone, total: transfer.bytesTotal },
  };
}

/**
 * Whether an event just made this transfer fail. The backend re-sends a
 * failed row, so only a row that was not already failing with the same
 * message has anything new to report.
 */
export function enteredMountFailure(
  previous: MountTransfer | undefined,
  next: MountTransfer | undefined
): boolean {
  if (!next || mountFailureRecord(next) === null) return false;
  return previous?.state !== 'error' || previous.error !== next.error;
}

/**
 * Keep the failure modal in step with a transfer: report the failure it just
 * entered, and end its record once it gets through, so the next failure is
 * news again. Only `done` ends it: the write-back retries itself after a
 * cooldown and announces waiting and active frames on the way, and a transient
 * failure repeating through those frames must not reopen the modal under the
 * reader every cycle.
 */
function trackMountFailure(previous: MountTransfer | undefined, next: MountTransfer | undefined) {
  const errors = useTransferErrorStore.getState();
  const failure = next && enteredMountFailure(previous, next) ? mountFailureRecord(next) : null;
  if (failure) errors.report(failure);
  else if (next?.state === 'done') errors.forget(`mount:${next.id}`);
}

function errorMessage(e: unknown): string {
  if (typeof e === 'string') return e;
  if (e instanceof Error) return e.message;
  return String(e);
}

export const useMountStore = create<MountStore>((set, get) => ({
  mounts: [],
  transfers: [],
  modalOpen: false,
  target: null,
  isMounting: false,
  isUnmounting: false,
  error: null,
  recoveries: [],
  recoveryError: null,
  isLoadingRecoveries: false,
  selectedRecoveryId: null,

  openMountModal: (target, recoveryId) =>
    set({
      modalOpen: true,
      target,
      error: null,
      selectedRecoveryId: get().recoveries.some(
        (r) => r.recoveryId === recoveryId && recoveryMatchesTarget(r, target)
      )
        ? recoveryId!
        : null,
    }),

  closeMountModal: () => set({ modalOpen: false, error: null }),

  setRecoverySelection: (recoveryId) => {
    const { target, recoveries } = get();
    set({
      selectedRecoveryId:
        target &&
        recoveries.some((r) => r.recoveryId === recoveryId && recoveryMatchesTarget(r, target))
          ? recoveryId
          : null,
    });
  },

  refreshRecoveries: async () => {
    if (get().isLoadingRecoveries) return;
    set({ isLoadingRecoveries: true, recoveryError: null });
    try {
      const payloads = await invoke<MountRecoveryPayload[]>('list_mount_recoveries');
      const recoveries = payloads.map(toMountRecovery);
      set((state) => ({
        recoveries,
        isLoadingRecoveries: false,
        selectedRecoveryId:
          state.target &&
          recoveries.some(
            (r) =>
              r.recoveryId === state.selectedRecoveryId && recoveryMatchesTarget(r, state.target!)
          )
            ? state.selectedRecoveryId
            : null,
      }));
    } catch (e) {
      set({ isLoadingRecoveries: false, recoveryError: errorMessage(e) });
    }
  },

  exportRecovery: async (recoveryId, destination) => {
    const recovery = get().recoveries.find((r) => r.recoveryId === recoveryId);
    if (!recovery || recovery.active) {
      set({ recoveryError: 'Unmount this bucket before exporting its saved writes.' });
      return null;
    }
    set({ recoveryError: null });
    try {
      return await invoke<string>('export_mount_recovery', { recoveryId, destination });
    } catch (e) {
      set({ recoveryError: errorMessage(e) });
      return null;
    }
  },

  discardRecovery: async (recoveryId) => {
    const recovery = get().recoveries.find((r) => r.recoveryId === recoveryId);
    if (!recovery || recovery.active) {
      set({ recoveryError: 'Unmount this bucket before discarding its saved writes.' });
      return false;
    }
    if (recovery.recoveryId.startsWith('legacy:') || recovery.error) {
      set({ recoveryError: 'Export unrecognized saved writes to recover them manually.' });
      return false;
    }
    set({ recoveryError: null });
    try {
      await invoke('discard_mount_recovery', { recoveryId });
      set((state) => ({
        recoveries: state.recoveries.filter((r) => r.recoveryId !== recoveryId),
        selectedRecoveryId:
          state.selectedRecoveryId === recoveryId ? null : state.selectedRecoveryId,
      }));
      return true;
    } catch (e) {
      set({ recoveryError: errorMessage(e) });
      return false;
    }
  },

  clearError: () => set({ error: null }),

  setMounts: (mounts) =>
    set((state) => ({
      mounts,
      transfers: pruneDeadMountTransfers(state.transfers, mounts),
    })),

  applyTransfer: (event) => {
    const row = (transfers: MountTransfer[]) => transfers.find((t) => t.id === event.transfer_id);
    const previous = row(get().transfers);
    set((state) => ({ transfers: applyTransferEvent(state.transfers, event, Date.now()) }));
    trackMountFailure(previous, row(get().transfers));
  },

  clearFinishedTransfers: () =>
    set((state) => ({
      transfers: state.transfers.filter((t) => t.state === 'waiting' || t.state === 'active'),
    })),

  refreshMounts: async () => {
    try {
      const mounts = await invoke<MountInfoPayload[]>('list_mounts');
      get().setMounts(mounts.map(toMountInfo));
    } catch (e) {
      console.error('Failed to list mounts:', e);
    }
  },

  mount: async (input) => {
    if (input.recovery_id) {
      const recovery = get().recoveries.find((r) => r.recoveryId === input.recovery_id);
      if (
        !recovery ||
        input.read_only ||
        !recoveryMatchesTarget(recovery, {
          provider: input.provider,
          accountId: input.account_id,
          bucket: input.bucket,
        })
      ) {
        set({
          error:
            'Choose valid saved writes for this account and bucket, and allow changes to resume uploads.',
        });
        return null;
      }
    }
    set({ isMounting: true, error: null });
    try {
      const payload = await invoke<MountInfoPayload>('mount_bucket', { input });
      const info = toMountInfo(payload);
      // Merge eagerly: `mount-changed` also lands, but the modal switches to
      // its mounted state on this return value alone.
      set((state) => ({
        mounts: [...state.mounts.filter((m) => m.mountId !== info.mountId), info],
        isMounting: false,
        recoveries: state.recoveries.map((r) =>
          r.recoveryId === input.recovery_id ? { ...r, active: true } : r
        ),
        selectedRecoveryId: null,
      }));
      return info;
    } catch (e) {
      set({ isMounting: false, error: errorMessage(e) });
      return null;
    }
  },

  unmount: async (mountId) => {
    set({ isUnmounting: true, error: null });
    try {
      await invoke('unmount_bucket', { mountId });
      set((state) => {
        const mounts = state.mounts.filter((m) => m.mountId !== mountId);
        return {
          mounts,
          // No more events will ever arrive for this mount; live rows left
          // behind would hold the transfer dock open forever.
          transfers: pruneDeadMountTransfers(state.transfers, mounts),
          isUnmounting: false,
        };
      });
      return true;
    } catch (e) {
      set({ isUnmounting: false, error: errorMessage(e) });
      return false;
    }
  },
}));

// ── Selectors ─────────────────────────────────────────────────────

/** The live mount for one bucket, or undefined. Buckets are keyed per account. */
export function findMount(
  mounts: MountInfo[],
  provider: MountProvider,
  accountId: string,
  bucket: string
): MountInfo | undefined {
  return mounts.find(
    (m) => m.provider === provider && m.accountId === accountId && m.bucket === bucket
  );
}

export function isBucketMounted(
  mounts: MountInfo[],
  provider: MountProvider,
  accountId: string,
  bucket: string
): boolean {
  return findMount(mounts, provider, accountId, bucket) !== undefined;
}

/** Ask the backend where this bucket would mount by default (e.g. `~/CloudMounts/photos`). */
export async function defaultMountPath(bucket: string): Promise<string> {
  return invoke<string>('default_mount_path', { bucket });
}

// ── Flush errors ──────────────────────────────────────────────────

/** How long one failing file stays quiet after it has been reported once. */
export const FLUSH_ERROR_QUIET_MS = 30_000;

const flushErrorsReported = new Map<string, number>();

/** Failures are deduped per file, and the same name in two mounts is two files. */
export function flushErrorKey(event: MountFlushErrorEvent): string {
  return `${event.mount_id}\u0000${event.key}`;
}

export function flushErrorMessage(event: MountFlushErrorEvent): string {
  return `Upload of "${event.key}" to ${event.bucket} failed: ${event.error}`;
}

/**
 * A file that cannot upload is retried on a timer, so its failure arrives
 * again and again. Report each file once per quiet window; entries that have
 * gone quiet are dropped on the way past, so the record cannot grow for as
 * long as the mount lives.
 */
export function shouldReportFlushError(
  reported: Map<string, number>,
  key: string,
  now: number,
  quietMs: number = FLUSH_ERROR_QUIET_MS
): boolean {
  for (const [seen, at] of reported) {
    if (now - at >= quietMs) reported.delete(seen);
  }
  if (reported.has(key)) return false;
  reported.set(key, now);
  return true;
}

/**
 * Setup the global mount listener and load the current mount list. Called once
 * on app initialization; survives component unmounts so the sidebar's mounted
 * indicators stay accurate whether or not the mount modal is open.
 */
export async function setupGlobalMountListeners(): Promise<void> {
  if (globalListenersSetup) return;
  globalListenersSetup = true;

  try {
    const unlistenChanged = await listen<MountChangedEvent>('mount-changed', (event) => {
      useMountStore.getState().setMounts(event.payload.mounts.map(toMountInfo));
    });
    globalUnlisteners.push(unlistenChanged);

    // Background uploads fail long after the user left the folder alone, so
    // the toast is the only place this surfaces.
    const unlistenFlushError = await listen<MountFlushErrorEvent>('mount-flush-error', (event) => {
      if (!shouldReportFlushError(flushErrorsReported, flushErrorKey(event.payload), Date.now())) {
        return;
      }
      useToastStore.getState().pushToast(flushErrorMessage(event.payload), 'error');
    });
    globalUnlisteners.push(unlistenFlushError);

    // Live progress for staged uploads and edit-time downloads, rendered by
    // the transfer dock alongside the app's own transfers.
    const unlistenTransfer = await listen<MountTransferEvent>('mount-transfer', (event) => {
      useMountStore.getState().applyTransfer(event.payload);
    });
    globalUnlisteners.push(unlistenTransfer);

    await useMountStore.getState().refreshMounts();
  } catch (e) {
    console.error('Failed to setup global mount listeners:', e);
    // Undo whatever did register before clearing the guard: a retry that found
    // `mount-changed` still subscribed would deliver every event twice.
    for (const unlisten of globalUnlisteners.splice(0)) {
      try {
        unlisten();
      } catch {
        /* already gone — nothing left to undo */
      }
    }
    globalListenersSetup = false;
  }
}
