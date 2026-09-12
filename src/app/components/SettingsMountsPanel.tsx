'use client';

import { useCallback, useEffect, useMemo, useState } from 'react';
import { App } from 'antd';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import { DisconnectOutlined, FolderOpenOutlined, LinkOutlined } from '@ant-design/icons';
import { ProviderIcon } from '@/app/components/AccountSidebarRows';
import {
  canResumeRecovery,
  recoveryStateLabel,
  resolveRecoveryTarget,
  useMountStore,
  type MountInfo,
  type MountRecovery,
} from '@/app/stores/mountStore';
import { useAccountStore } from '@/app/stores/accountStore';
import { formatBytes } from '@/app/utils/formatBytes';
import {
  detectOs,
  middleTruncate,
  mountModeLabel,
  relativeMountTime,
  revealActionLabel,
} from '@/app/utils/mount';

/** Relative times go stale while the panel sits open; re-read the clock. */
const CLOCK_TICK_MS = 30_000;

function useClock(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), CLOCK_TICK_MS);
    return () => clearInterval(id);
  }, []);
  return now;
}

/* ── One live mount ──────────────────────────────────────────────── */
function MountRow({
  mount,
  now,
  busy,
  revealLabel,
  onReveal,
  onUnmount,
}: {
  mount: MountInfo;
  now: number;
  busy: boolean;
  revealLabel: string;
  onReveal: (mount: MountInfo) => void;
  onUnmount: (mount: MountInfo) => void;
}) {
  return (
    <div className="settings-mount-card">
      <ProviderIcon provider={mount.provider} />

      <div className="settings-mount-card-body">
        {/* The mount schematic at list scale: bucket ─●─ folder */}
        <div className="settings-mount-link">
          <span className="settings-mount-bucket">{mount.bucket}</span>
          <span
            className={`mount-link-wire settings-mount-wire ${mount.health === 'mounted' ? 'mounted' : ''}`}
            aria-hidden="true"
          />
          <code className="settings-mount-path" title={mount.localPath}>
            {middleTruncate(mount.localPath)}
          </code>
        </div>

        <div className="settings-mount-foot">
          <div className="settings-mount-meta">
            <span
              className={['mount-tag', mount.readOnly ? '' : 'mount-tag-live'].join(' ').trim()}
            >
              {mountModeLabel(mount.readOnly)}
            </span>
            <span
              style={
                mount.health === 'offline' || mount.health === 'degraded'
                  ? { color: 'var(--accent)' }
                  : undefined
              }
            >
              {mount.health === 'mounted'
                ? `Mounted ${relativeMountTime(mount.mountedAt, now)}`
                : mount.health === 'degraded'
                  ? 'Degraded'
                  : mount.health === 'offline'
                    ? 'Offline'
                    : 'Unmounting…'}
            </span>
            {mount.pendingUploads > 0 && (
              <span>
                {mount.pendingUploads} pending {mount.pendingUploads === 1 ? 'upload' : 'uploads'}
              </span>
            )}
            <span className="settings-mount-dot" />
            <span className="settings-mount-account">{mount.accountId}</span>
          </div>

          <div className="settings-mount-actions">
            <button className="btn btn-sm" onClick={() => onReveal(mount)} disabled={busy}>
              <FolderOpenOutlined style={{ fontSize: 11 }} />
              {revealLabel}
            </button>
            <button className="btn btn-sm" onClick={() => onUnmount(mount)} disabled={busy}>
              <DisconnectOutlined style={{ fontSize: 11 }} />
              {busy ? 'Unmounting…' : 'Unmount'}
            </button>
          </div>
        </div>
        {mount.healthError && (
          <div className="field-hint" role="status">
            {mount.healthError}
          </div>
        )}
      </div>
    </div>
  );
}

function PendingMountUploads() {
  const recoveries = useMountStore((s) => s.recoveries);
  const recoveryError = useMountStore((s) => s.recoveryError);
  const loading = useMountStore((s) => s.isLoadingRecoveries);
  const refresh = useMountStore((s) => s.refreshRecoveries);
  const exportRecovery = useMountStore((s) => s.exportRecovery);
  const discardRecovery = useMountStore((s) => s.discardRecovery);
  const openMountModal = useMountStore((s) => s.openMountModal);
  const mountCount = useMountStore((s) => s.mounts.length);
  const accounts = useAccountStore((s) => s.accounts);
  const { message, modal } = App.useApp();
  const [busyId, setBusyId] = useState<string | null>(null);

  useEffect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), 10_000);
    return () => clearInterval(timer);
  }, [refresh, mountCount]);

  const handleResume = (recovery: MountRecovery) => {
    const target = resolveRecoveryTarget(recovery, accounts);
    if (target) openMountModal(target, recovery.recoveryId);
    else
      message.info(
        'Open this account and bucket in the sidebar, choose Mount as local drive, then select these saved writes. Restore the saved account first if it is missing.'
      );
  };

  const handleExport = async (recovery: MountRecovery) => {
    setBusyId(recovery.recoveryId);
    try {
      const destination = await openDialog({
        directory: true,
        multiple: false,
        title: 'Export saved writes to a folder',
      });
      if (typeof destination !== 'string') return;
      const path = await exportRecovery(recovery.recoveryId, destination);
      if (path) message.success(`Saved writes exported to ${path}`);
      else message.error(useMountStore.getState().recoveryError || 'Could not export saved writes');
    } catch (e) {
      message.error(e instanceof Error ? e.message : String(e));
    } finally {
      setBusyId(null);
    }
  };

  const handleDiscard = (recovery: MountRecovery) => {
    modal.confirm({
      title: 'Discard saved changes?',
      content: `Permanently remove the local recovery records for ${recovery.bucket || recovery.recoveryId}? Unuploaded files will be lost. Completed cloud changes will remain. Export a copy first if you need to keep these records.`,
      okText: 'Discard saved writes',
      okButtonProps: { danger: true },
      onOk: async () => {
        setBusyId(recovery.recoveryId);
        try {
          if (await discardRecovery(recovery.recoveryId)) message.success('Saved writes discarded');
          else
            throw new Error(
              useMountStore.getState().recoveryError || 'Could not discard saved writes'
            );
        } catch (e) {
          message.error(e instanceof Error ? e.message : String(e));
          throw e;
        } finally {
          setBusyId(null);
        }
      },
    });
  };

  return (
    <section className="settings-section">
      <div className="settings-section-head">
        <div>
          <h3>Pending changes</h3>
          <p>Resume interrupted uploads and file changes, or export their recovery files.</p>
        </div>
        <button className="btn btn-sm" onClick={() => void refresh()} disabled={loading}>
          {loading ? 'Refreshing…' : 'Refresh'}
        </button>
      </div>
      {recoveryError && (
        <div className="mount-error" role="alert">
          {recoveryError}
        </div>
      )}
      {recoveries.length === 0 ? (
        <p className="field-hint">
          {loading
            ? 'Checking saved writes…'
            : recoveryError
              ? 'Saved writes could not be checked.'
              : 'No saved writes waiting to upload.'}
        </p>
      ) : (
        <div className="settings-mount-list">
          {recoveries.map((recovery) => (
            <div className="settings-mount-card" key={recovery.recoveryId}>
              {recovery.provider ? (
                <ProviderIcon provider={recovery.provider} />
              ) : (
                <FolderOpenOutlined />
              )}
              <div className="settings-mount-card-body">
                <strong>{recovery.bucket || 'Unrecognized saved writes'}</strong>
                <div className="settings-mount-meta">
                  <span>
                    {recovery.files.length} {recovery.files.length === 1 ? 'file' : 'files'} ·{' '}
                    {formatBytes(recovery.files.reduce((sum, file) => sum + file.size, 0))}
                  </span>
                  {recovery.accountId && <span>{recovery.accountId}</span>}
                </div>
                <code className="settings-mount-path" title={recovery.path}>
                  {middleTruncate(recovery.path)}
                </code>
                {recovery.error && (
                  <div className="field-hint" role="status">
                    {recovery.error} Export these files to recover them manually.
                  </div>
                )}
                {recovery.active && (
                  <div className="field-hint">
                    This bucket is mounted. Uploads are managed by its active mount.
                  </div>
                )}
                <details>
                  <summary>Saved changes</summary>
                  <div style={{ maxHeight: 180, overflowY: 'auto' }}>
                    {recovery.files.slice(0, 50).map((file) => (
                      <div className="field-hint" key={file.path}>
                        <span>{file.key || file.path}</span> · {formatBytes(file.size)} ·{' '}
                        {recoveryStateLabel(file.state)}
                        {file.error && <div>{file.error}</div>}
                      </div>
                    ))}
                    {recovery.files.length > 50 && (
                      <div className="field-hint">
                        Showing the first 50 files. Export includes all saved files.
                      </div>
                    )}
                  </div>
                </details>
                <div className="settings-mount-actions">
                  {canResumeRecovery(recovery) && (
                    <button
                      className="btn btn-sm btn-primary"
                      disabled={busyId !== null}
                      onClick={() => handleResume(recovery)}
                    >
                      Resume uploads
                    </button>
                  )}
                  <button
                    className="btn btn-sm"
                    disabled={recovery.active || busyId !== null}
                    onClick={() => void handleExport(recovery)}
                  >
                    Export files…
                  </button>
                  {!recovery.error && (
                    <button
                      className="btn btn-sm"
                      disabled={recovery.active || busyId !== null}
                      onClick={() => handleDiscard(recovery)}
                    >
                      Discard…
                    </button>
                  )}
                </div>
              </div>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}

/* ── SettingsMountsPanel ─────────────────────────────────────────── */
export default function SettingsMountsPanel() {
  const mounts = useMountStore((s) => s.mounts);
  const unmountBucket = useMountStore((s) => s.unmount);
  const refreshMounts = useMountStore((s) => s.refreshMounts);

  useEffect(() => {
    void refreshMounts();
    const timer = setInterval(() => void refreshMounts(), 10_000);
    return () => clearInterval(timer);
  }, [refreshMounts]);

  const { message } = App.useApp();
  const os = useMemo(() => detectOs(), []);
  const revealLabel = useMemo(() => revealActionLabel(os), [os]);
  const now = useClock();

  // Busy is per row: unmounting one bucket must not freeze the others.
  const [busyIds, setBusyIds] = useState<ReadonlySet<string>>(() => new Set());

  const setBusy = useCallback((mountId: string, busy: boolean) => {
    setBusyIds((prev) => {
      const next = new Set(prev);
      if (busy) next.add(mountId);
      else next.delete(mountId);
      return next;
    });
  }, []);

  const handleReveal = useCallback(
    async (mount: MountInfo) => {
      try {
        const { revealItemInDir } = await import('@tauri-apps/plugin-opener');
        await revealItemInDir(mount.localPath);
      } catch {
        try {
          const { openPath } = await import('@tauri-apps/plugin-opener');
          await openPath(mount.localPath);
        } catch (e) {
          console.error('Failed to open mount folder:', e);
          message.error('Could not open the folder');
        }
      }
    },
    [message]
  );

  const unmountOne = useCallback(
    async (mount: MountInfo) => {
      setBusy(mount.mountId, true);
      const ok = await unmountBucket(mount.mountId);
      setBusy(mount.mountId, false);
      return ok;
    },
    [setBusy, unmountBucket]
  );

  const handleUnmount = useCallback(
    async (mount: MountInfo) => {
      const ok = await unmountOne(mount);
      if (ok) message.success(`${mount.bucket} unmounted`);
      else message.error(useMountStore.getState().error || `Could not unmount ${mount.bucket}`);
    },
    [message, unmountOne]
  );

  const handleUnmountAll = useCallback(async () => {
    let done = 0;
    for (const mount of [...mounts]) {
      const ok = await unmountOne(mount);
      if (!ok) {
        message.error(useMountStore.getState().error || `Could not unmount ${mount.bucket}`);
        break;
      }
      done += 1;
    }
    if (done > 0) message.success(`Unmounted ${done} ${done === 1 ? 'bucket' : 'buckets'}`);
  }, [message, mounts, unmountOne]);

  const anyBusy = busyIds.size > 0;

  return (
    <div className="settings-section-stack">
      <section className="settings-section">
        <div className="settings-section-head">
          <div>
            <h3>Mounted buckets</h3>
            <p>Buckets attached to this computer as local folders.</p>
          </div>
          <div className="settings-mount-head-actions">
            {mounts.length > 0 && (
              <span className="settings-pill">
                {mounts.length} {mounts.length === 1 ? 'mount' : 'mounts'}
              </span>
            )}
            {mounts.length >= 2 && (
              <button className="btn btn-sm" onClick={handleUnmountAll} disabled={anyBusy}>
                Unmount all
              </button>
            )}
          </div>
        </div>

        {mounts.length === 0 ? (
          <div className="settings-account-empty">
            <LinkOutlined style={{ fontSize: 22, color: 'var(--text-subtle)' }} />
            <strong>Nothing mounted</strong>
            <span>
              Open a bucket&rsquo;s menu in the sidebar and choose Mount as local drive. It shows up
              here once it is live.
            </span>
          </div>
        ) : (
          <div className="settings-mount-list">
            {mounts.map((mount) => (
              <MountRow
                key={mount.mountId}
                mount={mount}
                now={now}
                busy={busyIds.has(mount.mountId) || mount.health === 'unmounting'}
                revealLabel={revealLabel}
                onReveal={handleReveal}
                onUnmount={handleUnmount}
              />
            ))}
          </div>
        )}
      </section>
      <PendingMountUploads />
    </div>
  );
}
