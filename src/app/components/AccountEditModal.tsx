'use client';

import { useState, useEffect, useCallback } from 'react';
import { App } from 'antd';
import {
  DeleteOutlined,
  DatabaseOutlined,
  EyeOutlined,
  EyeInvisibleOutlined,
  ReloadOutlined,
  UserOutlined,
} from '@ant-design/icons';
import { invoke } from '@tauri-apps/api/core';
import { useAccountStore, type ProviderAccount } from '@/app/stores/accountStore';
import { listBuckets, type StorageProvider } from '@/app/lib/r2cache';
import Modal from '@/app/components/ui/Modal';

export interface AccountEditModalProps {
  open: boolean;
  /** When provided, edit this account; otherwise create a new account. */
  accountId?: string | null;
  /** Initial provider for new accounts. */
  initialProvider?: StorageProvider;
  onClose: () => void;
  /** Notifies the parent that the account list may have changed. */
  onChanged?: () => void;
}

interface BucketRow {
  name: string;
}

/* ── Provider picker ────────────────────────────────────────────── */
const PROVIDERS: { id: StorageProvider; label: string; desc: string; icon: string }[] = [
  { id: 'r2', label: 'Cloudflare R2', desc: 'Zero-egress', icon: 'R2' },
  { id: 'aws', label: 'AWS S3', desc: 'Region-based', icon: 'S3' },
  { id: 'minio', label: 'MinIO', desc: 'Self-hosted', icon: 'M' },
  { id: 'rustfs', label: 'RustFS', desc: 'Path-style', icon: 'RF' },
];

/* ── BucketListRow ───────────────────────────────────────────────── */
function BucketListRow({
  bucket,
  onDelete,
}: {
  bucket: BucketRow;
  onDelete: (name: string) => void;
}) {
  return (
    <div
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: 10,
        padding: '8px 12px',
        background: 'var(--bg-sunken)',
        borderRadius: 7,
        border: '1px solid var(--border)',
        fontSize: 12.5,
      }}
    >
      <DatabaseOutlined style={{ fontSize: 13, color: 'var(--text-muted)' }} />
      <span style={{ flex: 1, fontFamily: 'var(--font-mono)', fontSize: 12 }}>{bucket.name}</span>
      <button
        className="fl-act-btn danger"
        onClick={() => onDelete(bucket.name)}
        title="Remove bucket"
      >
        <DeleteOutlined style={{ fontSize: 12 }} />
      </button>
    </div>
  );
}

/* ── AccountEditModal ────────────────────────────────────────────── */
export default function AccountEditModal({
  open,
  accountId,
  initialProvider,
  onClose,
  onChanged,
}: AccountEditModalProps) {
  const { message, modal } = App.useApp();

  const accounts = useAccountStore((s) => s.accounts);
  const createAccount = useAccountStore((s) => s.createAccount);
  const updateAccount = useAccountStore((s) => s.updateAccount);
  const deleteAccount = useAccountStore((s) => s.deleteAccount);
  const createToken = useAccountStore((s) => s.createToken);
  const saveBuckets = useAccountStore((s) => s.saveBuckets);
  const createAwsAccount = useAccountStore((s) => s.createAwsAccount);
  const updateAwsAccount = useAccountStore((s) => s.updateAwsAccount);
  const deleteAwsAccount = useAccountStore((s) => s.deleteAwsAccount);
  const saveAwsBuckets = useAccountStore((s) => s.saveAwsBuckets);
  const createMinioAccount = useAccountStore((s) => s.createMinioAccount);
  const updateMinioAccount = useAccountStore((s) => s.updateMinioAccount);
  const deleteMinioAccount = useAccountStore((s) => s.deleteMinioAccount);
  const saveMinioBuckets = useAccountStore((s) => s.saveMinioBuckets);
  const createRustfsAccount = useAccountStore((s) => s.createRustfsAccount);
  const updateRustfsAccount = useAccountStore((s) => s.updateRustfsAccount);
  const deleteRustfsAccount = useAccountStore((s) => s.deleteRustfsAccount);
  const saveRustfsBuckets = useAccountStore((s) => s.saveRustfsBuckets);

  const isNew = !accountId;

  // Form state
  const [provider, setProvider] = useState<StorageProvider>(initialProvider ?? 'r2');
  const [accountName, setAccountName] = useState('');
  const [accountIdField, setAccountIdField] = useState(''); // R2 account ID
  const [region, setRegion] = useState('');
  const [endpointScheme, setEndpointScheme] = useState('https');
  const [endpointHost, setEndpointHost] = useState('');
  const [accessKeyId, setAccessKeyId] = useState('');
  const [secretAccessKey, setSecretAccessKey] = useState('');
  const [showSecret, setShowSecret] = useState(false);
  const [apiToken, setApiToken] = useState('');
  const [tokenName, setTokenName] = useState('');

  const [buckets, setBuckets] = useState<BucketRow[]>([]);
  const [loadingBuckets, setLoadingBuckets] = useState(false);
  const [saving, setSaving] = useState(false);

  // Reset and populate form whenever the modal opens or the target account changes
  useEffect(() => {
    if (!open) return;
    if (isNew) {
      setProvider(initialProvider ?? 'r2');
      setAccountName('');
      setAccountIdField('');
      setRegion('');
      setEndpointScheme('https');
      setEndpointHost('');
      setAccessKeyId('');
      setSecretAccessKey('');
      setApiToken('');
      setTokenName('');
      setBuckets([]);
      setShowSecret(false);
      return;
    }
    const acct = accounts.find((a) => a.account.id === accountId);
    if (!acct) return;
    setProvider(acct.provider);
    setAccountName(acct.account.name ?? '');
    setAccountIdField(acct.account.id);
    setShowSecret(false);

    if (acct.provider === 'r2') {
      const firstToken = acct.tokens[0];
      if (firstToken) {
        setAccessKeyId(firstToken.token.access_key_id);
        setSecretAccessKey(firstToken.token.secret_access_key);
        setApiToken(firstToken.token.api_token);
        setTokenName(firstToken.token.name ?? '');
        setBuckets(firstToken.buckets.map((b) => ({ name: b.name })));
      } else {
        setAccessKeyId('');
        setSecretAccessKey('');
        setApiToken('');
        setTokenName('');
        setBuckets([]);
      }
      setRegion('');
      setEndpointScheme('https');
      setEndpointHost('');
    } else if (acct.provider === 'aws') {
      setAccessKeyId(acct.account.access_key_id);
      setSecretAccessKey(acct.account.secret_access_key);
      setRegion(acct.account.region);
      setEndpointScheme(acct.account.endpoint_scheme);
      setEndpointHost(acct.account.endpoint_host ?? '');
      setBuckets(acct.buckets.map((b) => ({ name: b.name })));
    } else if (acct.provider === 'minio') {
      setAccessKeyId(acct.account.access_key_id);
      setSecretAccessKey(acct.account.secret_access_key);
      setEndpointScheme(acct.account.endpoint_scheme);
      setEndpointHost(acct.account.endpoint_host);
      setBuckets(acct.buckets.map((b) => ({ name: b.name })));
      setRegion('');
    } else if (acct.provider === 'rustfs') {
      setAccessKeyId(acct.account.access_key_id);
      setSecretAccessKey(acct.account.secret_access_key);
      setEndpointScheme(acct.account.endpoint_scheme);
      setEndpointHost(acct.account.endpoint_host);
      setBuckets(acct.buckets.map((b) => ({ name: b.name })));
      setRegion('');
    }
  }, [open, accountId, accounts, isNew, initialProvider]);

  function removeBucket(name: string) {
    setBuckets((prev) => prev.filter((b) => b.name !== name));
  }

  const handleLoadBuckets = useCallback(async () => {
    if (provider === 'r2' && (!accountIdField || !accessKeyId || !secretAccessKey)) {
      message.warning('Please enter Account ID, Access Key ID, and Secret Access Key first');
      return;
    }
    if (provider === 'aws' && (!accessKeyId || !secretAccessKey || !region)) {
      message.warning('Please enter Region, Access Key ID, and Secret Access Key first');
      return;
    }
    if (
      (provider === 'minio' || provider === 'rustfs') &&
      (!accessKeyId || !secretAccessKey || !endpointHost)
    ) {
      message.warning('Please enter Endpoint, Access Key ID, and Secret Access Key first');
      return;
    }

    setLoadingBuckets(true);
    try {
      const existingAccountId =
        !isNew && accountId
          ? accountId
          : provider === 'aws'
            ? 'aws'
            : provider === 'minio'
              ? 'minio'
              : provider === 'rustfs'
                ? 'rustfs'
                : accountIdField;

      const result = await listBuckets(
        provider === 'r2'
          ? {
              provider: 'r2',
              accountId: accountIdField,
              bucket: '',
              accessKeyId,
              secretAccessKey,
            }
          : provider === 'aws'
            ? {
                provider: 'aws',
                accountId: existingAccountId,
                bucket: '',
                accessKeyId,
                secretAccessKey,
                region,
                endpointScheme: endpointScheme || undefined,
                endpointHost: endpointHost || undefined,
                forcePathStyle: false,
              }
            : provider === 'minio'
              ? {
                  provider: 'minio',
                  accountId: existingAccountId,
                  bucket: '',
                  accessKeyId,
                  secretAccessKey,
                  endpointScheme,
                  endpointHost,
                  forcePathStyle: false,
                }
              : {
                  provider: 'rustfs',
                  accountId: existingAccountId,
                  bucket: '',
                  accessKeyId,
                  secretAccessKey,
                  endpointScheme,
                  endpointHost,
                  forcePathStyle: true,
                }
      );
      const merged = result.map((b) => {
        const existing = buckets.find((eb) => eb.name === b.name);
        return existing ?? { name: b.name };
      });
      setBuckets(merged);
      message.success(`Found ${merged.length} bucket(s)`);
    } catch (e) {
      message.error(e instanceof Error ? e.message : 'Failed to load buckets');
    } finally {
      setLoadingBuckets(false);
    }
  }, [
    provider,
    accountIdField,
    accessKeyId,
    secretAccessKey,
    region,
    endpointScheme,
    endpointHost,
    isNew,
    accountId,
    buckets,
    message,
  ]);

  async function handleSave() {
    if (!accountName.trim()) {
      message.warning('Account name is required');
      return;
    }

    setSaving(true);
    try {
      if (isNew) {
        if (buckets.length === 0) {
          message.warning('Add at least one bucket before saving');
          setSaving(false);
          return;
        }
        if (provider === 'r2') {
          if (!accountIdField.trim()) {
            message.warning('Account ID is required');
            setSaving(false);
            return;
          }
          await createAccount(accountIdField.trim(), accountName.trim());
          const token = await createToken({
            account_id: accountIdField.trim(),
            name: tokenName || undefined,
            api_token: apiToken,
            access_key_id: accessKeyId,
            secret_access_key: secretAccessKey,
          });
          await saveBuckets(
            token.id,
            buckets.map((b) => ({
              name: b.name,
              public_domain: null,
              public_domain_scheme: null,
            }))
          );
        } else if (provider === 'aws') {
          if (!region.trim()) {
            message.warning('Region is required');
            setSaving(false);
            return;
          }
          const acct = await createAwsAccount({
            name: accountName.trim(),
            access_key_id: accessKeyId,
            secret_access_key: secretAccessKey,
            region: region.trim(),
            endpoint_scheme: endpointScheme || null,
            endpoint_host: endpointHost || null,
            force_path_style: false,
          });
          await saveAwsBuckets(
            acct.id,
            buckets.map((b) => ({
              name: b.name,
              public_domain_scheme: null,
              public_domain_host: null,
            }))
          );
        } else if (provider === 'minio') {
          if (!endpointHost.trim()) {
            message.warning('Endpoint host is required');
            setSaving(false);
            return;
          }
          const acct = await createMinioAccount({
            name: accountName.trim(),
            access_key_id: accessKeyId,
            secret_access_key: secretAccessKey,
            endpoint_scheme: endpointScheme || 'https',
            endpoint_host: endpointHost.trim(),
            force_path_style: false,
          });
          await saveMinioBuckets(
            acct.id,
            buckets.map((b) => ({ name: b.name }))
          );
        } else if (provider === 'rustfs') {
          if (!endpointHost.trim()) {
            message.warning('Endpoint host is required');
            setSaving(false);
            return;
          }
          const acct = await createRustfsAccount({
            name: accountName.trim(),
            access_key_id: accessKeyId,
            secret_access_key: secretAccessKey,
            endpoint_scheme: endpointScheme || 'https',
            endpoint_host: endpointHost.trim(),
          });
          await saveRustfsBuckets(
            acct.id,
            buckets.map((b) => ({ name: b.name }))
          );
        }
        message.success('Account created');
        onChanged?.();
        onClose();
      } else {
        const acct = accounts.find((a) => a.account.id === accountId);
        if (!acct) return;
        if (acct.provider === 'r2') {
          await updateAccount(acct.account.id, accountName.trim());
          const firstToken = acct.tokens[0];
          if (firstToken) {
            await invoke('update_token', {
              input: {
                id: firstToken.token.id,
                name: tokenName || null,
                api_token: apiToken,
                access_key_id: accessKeyId,
                secret_access_key: secretAccessKey,
              },
            });
            await saveBuckets(
              firstToken.token.id,
              buckets.map((b) => ({
                name: b.name,
                public_domain: null,
                public_domain_scheme: null,
              }))
            );
          }
        } else if (acct.provider === 'aws') {
          await updateAwsAccount({
            id: acct.account.id,
            name: accountName.trim(),
            access_key_id: accessKeyId,
            secret_access_key: secretAccessKey,
            region: region.trim(),
            endpoint_scheme: endpointScheme || null,
            endpoint_host: endpointHost || null,
            force_path_style: false,
          });
          await saveAwsBuckets(
            acct.account.id,
            buckets.map((b) => ({
              name: b.name,
              public_domain_scheme: null,
              public_domain_host: null,
            }))
          );
        } else if (acct.provider === 'minio') {
          await updateMinioAccount({
            id: acct.account.id,
            name: accountName.trim(),
            access_key_id: accessKeyId,
            secret_access_key: secretAccessKey,
            endpoint_scheme: endpointScheme || 'https',
            endpoint_host: endpointHost.trim(),
            force_path_style: false,
          });
          await saveMinioBuckets(
            acct.account.id,
            buckets.map((b) => ({ name: b.name }))
          );
        } else if (acct.provider === 'rustfs') {
          await updateRustfsAccount({
            id: acct.account.id,
            name: accountName.trim(),
            access_key_id: accessKeyId,
            secret_access_key: secretAccessKey,
            endpoint_scheme: endpointScheme || 'https',
            endpoint_host: endpointHost.trim(),
          });
          await saveRustfsBuckets(
            acct.account.id,
            buckets.map((b) => ({ name: b.name }))
          );
        }
        message.success('Account updated');
        onChanged?.();
        onClose();
      }
    } catch (e) {
      message.error(e instanceof Error ? e.message : 'Save failed');
    } finally {
      setSaving(false);
    }
  }

  function handleDelete() {
    if (isNew || !accountId) return;
    const acct = accounts.find((a) => a.account.id === accountId);
    if (!acct) return;
    modal.confirm({
      title: 'Delete Account',
      content: 'Delete this account and all its tokens and bucket configurations?',
      okText: 'Delete',
      okButtonProps: { danger: true },
      onOk: async () => {
        try {
          if (acct.provider === 'r2') await deleteAccount(acct.account.id);
          else if (acct.provider === 'aws') await deleteAwsAccount(acct.account.id);
          else if (acct.provider === 'minio') await deleteMinioAccount(acct.account.id);
          else await deleteRustfsAccount(acct.account.id);
          message.success('Account deleted');
          onChanged?.();
          onClose();
        } catch (e) {
          message.error(e instanceof Error ? e.message : 'Delete failed');
        }
      },
    });
  }

  const footer = (
    <>
      {!isNew && (
        <button
          className="btn btn-danger-ghost"
          style={{ marginRight: 'auto' }}
          onClick={handleDelete}
        >
          <DeleteOutlined style={{ fontSize: 12 }} />
          Delete account
        </button>
      )}
      <button className="btn" onClick={onClose}>
        Cancel
      </button>
      <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
        {saving ? 'Saving…' : isNew ? 'Create account' : 'Save changes'}
      </button>
    </>
  );

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={isNew ? 'New account' : 'Edit account'}
      subtitle={
        isNew
          ? 'Add a new storage provider connection'
          : 'Update credentials, buckets, or remove this account'
      }
      icon={<UserOutlined style={{ fontSize: 18 }} />}
      width={640}
      footer={footer}
    >
      <div className="settings-account-pad">
        {/* Provider picker (only for new account) */}
        {isNew && (
          <div className="field">
            <div className="field-label">Provider</div>
            <div className="provider-tags">
              {PROVIDERS.map((p) => (
                <div
                  key={p.id}
                  className={['provider-tag', provider === p.id ? 'active' : '']
                    .filter(Boolean)
                    .join(' ')}
                  onClick={() => setProvider(p.id)}
                >
                  <div className={`pi pi-${p.id}`}>{p.icon}</div>
                  <div>
                    <div className="pt-name">{p.label}</div>
                    <div className="pt-desc">{p.desc}</div>
                  </div>
                </div>
              ))}
            </div>
          </div>
        )}

        {/* Provider badge when editing */}
        {!isNew && (
          <div className="field">
            <div className="field-label">Provider</div>
            <div className="provider-tags">
              {PROVIDERS.filter((p) => p.id === provider).map((p) => (
                <div key={p.id} className="provider-tag active" style={{ cursor: 'default' }}>
                  <div className={`pi pi-${p.id}`}>{p.icon}</div>
                  <div>
                    <div className="pt-name">{p.label}</div>
                    <div className="pt-desc">{p.desc}</div>
                  </div>
                </div>
              ))}
            </div>
          </div>
        )}

        {/* Account name + provider-specific ID field */}
        <div className="field-row">
          <div className="field">
            <div className="field-label field-required">Account name</div>
            <input
              className="input"
              value={accountName}
              onChange={(e) => setAccountName(e.target.value)}
              placeholder="My account"
            />
          </div>
          {provider === 'r2' && isNew && (
            <div className="field">
              <div className="field-label field-required">Account ID</div>
              <input
                className="input mono"
                value={accountIdField}
                onChange={(e) => setAccountIdField(e.target.value)}
                placeholder="d8c7e60a3f5b4c1e…"
              />
            </div>
          )}
          {provider === 'aws' && (
            <div className="field">
              <div className="field-label field-required">Region</div>
              <input
                className="input mono"
                value={region}
                onChange={(e) => setRegion(e.target.value)}
                placeholder="us-east-1"
              />
            </div>
          )}
        </div>

        {/* Endpoint fields for MinIO / RustFS */}
        {(provider === 'minio' || provider === 'rustfs') && (
          <div className="field-row">
            <div className="field">
              <div className="field-label">Endpoint scheme</div>
              <select
                className="select"
                value={endpointScheme}
                onChange={(e) => setEndpointScheme(e.target.value)}
              >
                <option value="https">https</option>
                <option value="http">http</option>
              </select>
            </div>
            <div className="field" style={{ flex: 2 }}>
              <div className="field-label field-required">Endpoint host</div>
              <input
                className="input mono"
                value={endpointHost}
                onChange={(e) => setEndpointHost(e.target.value)}
                placeholder="minio.local:9000"
              />
            </div>
          </div>
        )}

        {/* R2 token fields (new R2 account only) */}
        {provider === 'r2' && isNew && (
          <div className="field-row">
            <div className="field">
              <div className="field-label">Token name</div>
              <input
                className="input"
                value={tokenName}
                onChange={(e) => setTokenName(e.target.value)}
                placeholder="My token"
              />
            </div>
            <div className="field">
              <div className="field-label field-required">API Token</div>
              <input
                className="input mono"
                value={apiToken}
                onChange={(e) => setApiToken(e.target.value)}
                placeholder="v3.eyJ…"
              />
            </div>
          </div>
        )}

        {/* Access key + secret */}
        <div className="field-row">
          <div className="field">
            <div className="field-label field-required">Access Key ID</div>
            <input
              className="input mono"
              value={accessKeyId}
              onChange={(e) => setAccessKeyId(e.target.value)}
              placeholder="AKIA…"
            />
          </div>
          <div className="field">
            <div className="field-label field-required">Secret Access Key</div>
            <div style={{ position: 'relative', display: 'flex', alignItems: 'center' }}>
              <input
                className="input mono"
                type={showSecret ? 'text' : 'password'}
                value={secretAccessKey}
                onChange={(e) => setSecretAccessKey(e.target.value)}
                placeholder="••••••••••••••••"
                style={{ paddingRight: 32, flex: 1 }}
              />
              <button
                className="fl-act-btn"
                style={{ position: 'absolute', right: 6 }}
                onClick={() => setShowSecret((s) => !s)}
                title={showSecret ? 'Hide' : 'Show'}
                type="button"
              >
                {showSecret ? (
                  <EyeInvisibleOutlined style={{ fontSize: 12 }} />
                ) : (
                  <EyeOutlined style={{ fontSize: 12 }} />
                )}
              </button>
            </div>
          </div>
        </div>

        {/* Buckets */}
        <div className="field" style={{ marginTop: 6 }}>
          <div className="field-label">Buckets ({buckets.length})</div>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
            {buckets.map((b) => (
              <BucketListRow key={b.name} bucket={b} onDelete={removeBucket} />
            ))}
            <button
              className="btn btn-sm"
              style={{ alignSelf: 'flex-start', marginTop: 4 }}
              onClick={handleLoadBuckets}
              disabled={loadingBuckets}
            >
              <ReloadOutlined spin={loadingBuckets} style={{ fontSize: 11 }} />
              Load buckets from API
            </button>
          </div>
        </div>
      </div>
    </Modal>
  );
}
