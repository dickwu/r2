'use client';

import { t as i18nT } from '@/i18n';
import { useTranslation } from 'react-i18next';
import { useCallback, useEffect, useMemo, useState } from 'react';
import { App } from 'antd';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import {
  CheckOutlined,
  CloudUploadOutlined,
  CopyOutlined,
  FolderOpenOutlined,
  LinkOutlined,
  LockOutlined,
} from '@ant-design/icons';
import Modal from '@/app/components/ui/Modal';
import { ProviderIcon } from '@/app/components/AccountSidebarRows';
import {
  defaultMountPath,
  findMount,
  recoveryMatchesTarget,
  useMountStore,
  type MountBucketInput,
  type MountProvider,
  type MountInfo,
  type MountRecovery,
} from '@/app/stores/mountStore';
import {
  detectOs,
  extractSudoCommand,
  joinMountPath,
  messageWithoutSudoCommand,
  mountModeHint,
  mountModeLabel,
  mountRequirementHint,
  pathLeaf,
  revealActionLabel,
} from '@/app/utils/mount';

const PROVIDER_NAME: Record<MountProvider, string> = {
  r2: 'R2',
  aws: 'S3',
  minio: 'MinIO',
  rustfs: 'RustFS',
};

function MountedBucketDetails({ mount }: { mount: MountInfo }) {
  const { t } = useTranslation();
  return (
    <>
      <div className="field">
        <div className="field-label">
          {mount.health === 'mounted'
            ? t('mount.mountedAtLabel')
            : mount.health === 'degraded'
              ? t('mount.degradedMount')
              : mount.health === 'offline'
                ? t('mount.mountOffline')
                : t('mount.unmounting')}
        </div>
        <div className="mount-path-readout">
          {mount.health === 'mounted' && <span className="mount-live-dot" aria-hidden="true" />}
          <code>{mount.localPath}</code>
        </div>
        {mount.healthError && (
          <div className="mount-error" role="status">
            {mount.healthError}
          </div>
        )}
        {mount.pendingUploads > 0 && (
          <div className="field-hint">
            {t('mount.pendingUploads', { count: mount.pendingUploads })}. {t('mount.savedRemain')}
          </div>
        )}
        <div className="field-hint">{t('mount.openLikeFolder')}</div>
      </div>

      <div className="mount-notes">
        <div className="mount-note">
          <span
            className={['mount-tag', mount.readOnly ? '' : 'mount-tag-live']
              .filter(Boolean)
              .join(' ')}
          >
            {mount.readOnly ? (
              <LockOutlined style={{ fontSize: 10 }} />
            ) : (
              <CloudUploadOutlined style={{ fontSize: 10 }} />
            )}{' '}
            {mountModeLabel(mount.readOnly)}
          </span>
          <span>{mountModeHint(mount.readOnly)}</span>
        </div>
        {!mount.readOnly && (
          <div className="mount-note mount-note-quiet">{t('mount.dropHint')}</div>
        )}
      </div>
    </>
  );
}

function SavedWritesSelector({
  recoveries,
  selectedId,
  busy,
  onSelect,
}: {
  recoveries: MountRecovery[];
  selectedId: string | null;
  busy: boolean;
  onSelect: (id: string | null) => void;
}) {
  const { t } = useTranslation();
  if (recoveries.length === 0) return null;
  return (
    <div className="field">
      <label className="field-label" htmlFor="mount-recovery">
        {t('mount.savedWrites')}
      </label>
      <select
        id="mount-recovery"
        className="input"
        value={selectedId ?? ''}
        disabled={busy}
        onChange={(event) => onSelect(event.target.value || null)}
      >
        <option value="">{t('mount.startNew')}</option>
        {recoveries.map((recovery) => (
          <option key={recovery.recoveryId} value={recovery.recoveryId}>
            {t('mount.savedFileCount', { count: recovery.files.length })} · {recovery.recoveryId}
          </option>
        ))}
      </select>
      <div className="field-hint">{t('mount.resumeHint')}</div>
    </div>
  );
}

export default function MountModal() {
  const target = useMountStore((s) => s.target);
  const isMounting = useMountStore((s) => s.isMounting);
  const isUnmounting = useMountStore((s) => s.isUnmounting);
  const error = useMountStore((s) => s.error);
  const closeMountModal = useMountStore((s) => s.closeMountModal);
  const mountBucket = useMountStore((s) => s.mount);
  const recoveries = useMountStore((s) => s.recoveries);
  const recoveryError = useMountStore((s) => s.recoveryError);
  const selectedRecoveryId = useMountStore((s) => s.selectedRecoveryId);
  const setRecoverySelection = useMountStore((s) => s.setRecoverySelection);
  const refreshRecoveries = useMountStore((s) => s.refreshRecoveries);
  const unmountBucket = useMountStore((s) => s.unmount);
  const mount = useMountStore((s) =>
    s.target
      ? findMount(s.mounts, s.target.provider, s.target.accountId, s.target.bucket)
      : undefined
  );

  const { message } = App.useApp();
  const { t } = useTranslation();
  const os = useMemo(() => detectOs(), []);
  const [localPath, setLocalPath] = useState('');
  const [readOnly, setReadOnly] = useState(false);
  const [copied, setCopied] = useState(false);

  const bucket = target?.bucket ?? '';
  const targetProvider = target?.provider;
  const targetAccountId = target?.accountId;
  const matchingRecoveries = useMemo(
    () => (target ? recoveries.filter((r) => recoveryMatchesTarget(r, target)) : []),
    [recoveries, target]
  );

  useEffect(() => {
    void refreshRecoveries();
  }, [refreshRecoveries, bucket, targetProvider, targetAccountId]);

  // The mode belongs to the mount, not to the dialog: a different bucket starts
  // from the writable default rather than inheriting the last choice.
  useEffect(() => {
    setReadOnly(false);
  }, [bucket, targetProvider, targetAccountId]);

  // Prefill the path: an existing mount's own path, otherwise the backend's default.
  useEffect(() => {
    if (!bucket) return;
    if (mount) {
      setLocalPath(mount.localPath);
      return;
    }
    let cancelled = false;
    defaultMountPath(bucket)
      .then((path) => {
        if (!cancelled) setLocalPath((current) => current || path);
      })
      .catch(() => {
        /* leave the field empty — the user can still pick a folder */
      });
    return () => {
      cancelled = true;
    };
  }, [bucket, mount]);

  const handleChooseFolder = useCallback(async () => {
    try {
      const picked = await openDialog({ directory: true, multiple: false });
      if (typeof picked === 'string') setLocalPath(joinMountPath(picked, bucket, os));
    } catch (e) {
      console.error('Failed to open folder picker:', e);
    }
  }, [bucket, os]);

  const handleMount = useCallback(async () => {
    if (!target || !localPath.trim()) return;
    const input: MountBucketInput = {
      provider: target.provider,
      account_id: target.accountId,
      bucket: target.bucket,
      local_path: localPath.trim(),
      access_key_id: target.accessKeyId,
      secret_access_key: target.secretAccessKey,
      region: target.region ?? null,
      endpoint_url: target.endpointUrl ?? null,
      force_path_style: target.forcePathStyle ?? null,
      read_only: selectedRecoveryId ? false : readOnly,
      recovery_id: selectedRecoveryId,
    };
    const info = await mountBucket(input);
    if (info)
      message.success(
        selectedRecoveryId
          ? i18nT('mount.resumed', { bucket: target.bucket, path: info.localPath })
          : i18nT('mount.mountedToast', { bucket: target.bucket, path: info.localPath })
      );
  }, [target, localPath, readOnly, selectedRecoveryId, mountBucket, message]);

  const handleUnmount = useCallback(async () => {
    if (!mount) return;
    const ok = await unmountBucket(mount.mountId);
    if (ok) message.success(i18nT('sidebar.unmounted', { bucket: mount.bucket }));
  }, [mount, unmountBucket, message]);

  const handleReveal = useCallback(async () => {
    if (!mount) return;
    try {
      const { revealItemInDir } = await import('@tauri-apps/plugin-opener');
      await revealItemInDir(mount.localPath);
    } catch {
      try {
        const { openPath } = await import('@tauri-apps/plugin-opener');
        await openPath(mount.localPath);
      } catch (e) {
        console.error('Failed to open mount folder:', e);
        message.error(i18nT('sidebar.openFolderFailed'));
      }
    }
  }, [mount, message]);

  const sudoCommand = useMemo(() => extractSudoCommand(error), [error]);
  // The command gets its own copyable block, so keep it out of the prose.
  const errorProse = sudoCommand ? messageWithoutSudoCommand(error) : error;

  const handleCopyCommand = useCallback(async () => {
    if (!sudoCommand) return;
    try {
      await navigator.clipboard.writeText(sudoCommand);
      setCopied(true);
      setTimeout(() => setCopied(false), 1600);
    } catch {
      message.error(i18nT('mount.copyCommandFailed'));
    }
  }, [sudoCommand, message]);

  if (!target) return null;

  const wireClass = [
    'mount-link-wire',
    mount?.health === 'mounted' ? 'mounted' : isMounting ? 'mounting' : '',
  ]
    .filter(Boolean)
    .join(' ');

  const footer = mount ? (
    <>
      <button
        className="btn"
        onClick={handleUnmount}
        disabled={isUnmounting || mount.health === 'unmounting'}
      >
        {isUnmounting || mount.health === 'unmounting'
          ? t('mount.unmounting')
          : t('sidebar.unmount')}
      </button>
      <button className="btn btn-primary" onClick={handleReveal}>
        <FolderOpenOutlined /> {revealActionLabel(os)}
      </button>
    </>
  ) : (
    <>
      <button className="btn" onClick={closeMountModal} disabled={isMounting}>
        {t('common.cancel')}
      </button>
      <button
        className="btn btn-primary"
        onClick={handleMount}
        disabled={isMounting || !localPath.trim()}
      >
        {isMounting
          ? t('mount.mounting')
          : selectedRecoveryId
            ? t('mount.resumeUploads')
            : t('mount.mount')}
      </button>
    </>
  );

  return (
    <Modal
      open={true}
      onClose={closeMountModal}
      title={t('mount.title')}
      subtitle={`${PROVIDER_NAME[target.provider]} · ${target.accountLabel}`}
      icon={<LinkOutlined style={{ fontSize: 18 }} />}
      width={560}
      footer={footer}
    >
      {/* Signature element: bucket ──link── local folder */}
      <div className="mount-link">
        <div className="mount-link-node">
          <ProviderIcon provider={target.provider} />
          <span className="mount-link-text">{target.bucket}</span>
        </div>
        <div className={wireClass} aria-hidden="true" />
        <div className="mount-link-node">
          <FolderOpenOutlined className="mount-link-icon" />
          <span className="mount-link-text">
            {localPath ? pathLeaf(localPath) : t('mount.localFolder')}
          </span>
        </div>
      </div>

      {mount ? (
        <MountedBucketDetails mount={mount} />
      ) : (
        <>
          <SavedWritesSelector
            recoveries={matchingRecoveries}
            selectedId={selectedRecoveryId}
            busy={isMounting}
            onSelect={(id) => {
              setRecoverySelection(id);
              if (id) setReadOnly(false);
            }}
          />
          {recoveryError && (
            <div className="field-hint" role="status">
              {t('mount.checkFailedWith', { error: recoveryError })}
            </div>
          )}
          <div className="field">
            <div className="field-label">{t('mount.localPath')}</div>
            <div className="mount-path-row">
              <input
                className="input mono"
                value={localPath}
                onChange={(e) => setLocalPath(e.target.value)}
                placeholder={t('mount.localPathPlaceholder')}
                aria-label={t('mount.localPath')}
              />
              <button className="btn" onClick={handleChooseFolder} disabled={isMounting}>
                {t('mount.chooseFolder')}
              </button>
            </div>
            <div className="field-hint">{t('mount.chooseFolderHint')}</div>
          </div>

          <div className="field">
            <button
              className={['toggle-row', !readOnly && 'on'].filter(Boolean).join(' ')}
              onClick={() => setReadOnly((v) => !v)}
              disabled={isMounting || !!selectedRecoveryId}
              aria-pressed={!readOnly}
            >
              <span className="option-row-text">
                <strong>{t('mount.allowChanges')}</strong>
                <span>{mountModeHint(readOnly)}</span>
              </span>
              <span className="toggle-switch">
                <span className="toggle-knob" />
              </span>
            </button>
          </div>

          <div className="mount-notes">
            <div className="mount-note mount-note-quiet">{mountRequirementHint(os)}</div>
          </div>
        </>
      )}

      {error && (
        <div className="mount-error">
          {errorProse && <div className="mount-error-text">{errorProse}</div>}
          {sudoCommand && (
            <>
              <div className="mount-cmd">
                <code>{sudoCommand}</code>
                <button
                  className="btn btn-sm"
                  onClick={handleCopyCommand}
                  aria-label={t('mount.copyCommand')}
                >
                  {copied ? <CheckOutlined /> : <CopyOutlined />}
                  {copied ? t('common.copied') : t('common.copy')}
                </button>
              </div>
              <div className="field-hint">{t('mount.runInTerminal')}</div>
            </>
          )}
        </div>
      )}
    </Modal>
  );
}
