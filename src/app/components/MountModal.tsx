'use client';

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
  useMountStore,
  type MountBucketInput,
  type MountProvider,
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

export default function MountModal() {
  const target = useMountStore((s) => s.target);
  const isMounting = useMountStore((s) => s.isMounting);
  const isUnmounting = useMountStore((s) => s.isUnmounting);
  const error = useMountStore((s) => s.error);
  const closeMountModal = useMountStore((s) => s.closeMountModal);
  const mountBucket = useMountStore((s) => s.mount);
  const unmountBucket = useMountStore((s) => s.unmount);
  const mount = useMountStore((s) =>
    s.target
      ? findMount(s.mounts, s.target.provider, s.target.accountId, s.target.bucket)
      : undefined
  );

  const { message } = App.useApp();
  const os = useMemo(() => detectOs(), []);
  const [localPath, setLocalPath] = useState('');
  const [readOnly, setReadOnly] = useState(false);
  const [copied, setCopied] = useState(false);

  const bucket = target?.bucket ?? '';
  const targetProvider = target?.provider;
  const targetAccountId = target?.accountId;

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
      read_only: readOnly,
    };
    const info = await mountBucket(input);
    if (info) message.success(`${target.bucket} mounted at ${info.localPath}`);
  }, [target, localPath, readOnly, mountBucket, message]);

  const handleUnmount = useCallback(async () => {
    if (!mount) return;
    const ok = await unmountBucket(mount.mountId);
    if (ok) message.success(`${mount.bucket} unmounted`);
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
        message.error('Could not open the folder');
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
      message.error('Could not copy the command');
    }
  }, [sudoCommand, message]);

  if (!target) return null;

  const wireClass = ['mount-link-wire', mount ? 'mounted' : isMounting ? 'mounting' : '']
    .filter(Boolean)
    .join(' ');

  const footer = mount ? (
    <>
      <button className="btn" onClick={handleUnmount} disabled={isUnmounting}>
        {isUnmounting ? 'Unmounting…' : 'Unmount'}
      </button>
      <button className="btn btn-primary" onClick={handleReveal}>
        <FolderOpenOutlined /> {revealActionLabel(os)}
      </button>
    </>
  ) : (
    <>
      <button className="btn" onClick={closeMountModal} disabled={isMounting}>
        Cancel
      </button>
      <button
        className="btn btn-primary"
        onClick={handleMount}
        disabled={isMounting || !localPath.trim()}
      >
        {isMounting ? 'Mounting…' : 'Mount'}
      </button>
    </>
  );

  return (
    <Modal
      open={true}
      onClose={closeMountModal}
      title="Mount bucket"
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
            {localPath ? pathLeaf(localPath) : 'Local folder'}
          </span>
        </div>
      </div>

      {mount ? (
        <>
          <div className="field">
            <div className="field-label">Mounted at</div>
            <div className="mount-path-readout">
              <span className="mount-live-dot" aria-hidden="true" />
              <code>{mount.localPath}</code>
            </div>
            <div className="field-hint">
              Open it like any folder. Unmount before deleting or moving it.
            </div>
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
              <div className="mount-note mount-note-quiet">
                Files you drop here upload in the background — keep the app running until they
                finish.
              </div>
            )}
          </div>
        </>
      ) : (
        <>
          <div className="field">
            <div className="field-label">Local path</div>
            <div className="mount-path-row">
              <input
                className="input mono"
                value={localPath}
                onChange={(e) => setLocalPath(e.target.value)}
                placeholder="Where the bucket should appear"
                aria-label="Local path"
              />
              <button className="btn" onClick={handleChooseFolder} disabled={isMounting}>
                Choose folder…
              </button>
            </div>
            <div className="field-hint">Choosing a folder adds the bucket name to the end.</div>
          </div>

          <div className="field">
            <button
              className={['toggle-row', !readOnly && 'on'].filter(Boolean).join(' ')}
              onClick={() => setReadOnly((v) => !v)}
              disabled={isMounting}
              aria-pressed={!readOnly}
            >
              <span className="option-row-text">
                <strong>Allow changes</strong>
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
                  aria-label="Copy command"
                >
                  {copied ? <CheckOutlined /> : <CopyOutlined />}
                  {copied ? 'Copied' : 'Copy'}
                </button>
              </div>
              <div className="field-hint">Run this in a terminal, then retry.</div>
            </>
          )}
        </div>
      )}
    </Modal>
  );
}
