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
  GlobalOutlined,
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
  /** When true, objects are served via direct (unsigned) public URLs. */
  isPublic: boolean;
  /** Public domain host (no scheme), e.g. "cdn.example.com". R2 only. */
  publicDomainHost: string;
  /** "https" | "http". Only meaningful when a host is set. */
  publicDomainScheme: string;
  /** R2 only: optional path segment prepended to the object key. */
  publicPathPrefix: string;
}

/* ── Provider picker ────────────────────────────────────────────── */
const PROVIDERS: { id: StorageProvider; label: string; desc: string; icon: string }[] = [
  { id: 'r2', label: 'Cloudflare R2', desc: 'Zero-egress', icon: 'R2' },
  { id: 'aws', label: 'AWS S3', desc: 'Region-based', icon: 'S3' },
  { id: 'minio', label: 'MinIO', desc: 'Self-hosted', icon: 'M' },
  { id: 'rustfs', label: 'RustFS', desc: 'Path-style', icon: 'RF' },
];

/* ── Domain helpers ──────────────────────────────────────────────── */
/** Split a pasted value like "https://cdn.example.com/" into scheme + host. */
function parseDomainInput(value: string): { scheme?: string; host: string } {
  const trimmed = value.trim();
  if (!trimmed) return { host: '' };
  if (trimmed.includes('://')) {
    try {
      const url = new URL(trimmed);
      // The scheme picker only offers http/https — clamp anything else so the
      // displayed and persisted scheme can't diverge.
      const scheme = url.protocol.replace(':', '') === 'http' ? 'http' : 'https';
      return { scheme, host: url.host };
    } catch {
      return { host: trimmed.replace(/\/+$/, '') };
    }
  }
  return { host: trimmed.replace(/\/+$/, '') };
}

/** Build the public base URL a bucket's files will resolve to, or null if private. */
function buildDomainPreview(scheme: string, host: string): string | null {
  const cleanHost = host
    .trim()
    .replace(/^https?:\/\//, '')
    .replace(/\/+$/, '');
  if (!cleanHost) return null;
  return `${scheme || 'https'}://${cleanHost}`;
}

/* ── BucketListRow ───────────────────────────────────────────────── */
function BucketListRow({
  bucket,
  provider,
  onDelete,
  onTogglePublic,
  onHostChange,
  onSchemeChange,
  onPrefixChange,
}: {
  bucket: BucketRow;
  provider: StorageProvider;
  onDelete: (name: string) => void;
  onTogglePublic: (name: string, value: boolean) => void;
  onHostChange: (name: string, value: string) => void;
  onSchemeChange: (name: string, scheme: string) => void;
  onPrefixChange: (name: string, value: string) => void;
}) {
  const isR2 = provider === 'r2';
  const isPublic = bucket.isPublic;
  const base = buildDomainPreview(bucket.publicDomainScheme, bucket.publicDomainHost);
  const cleanPrefix = bucket.publicPathPrefix.trim().replace(/^\/+|\/+$/g, '');
  const previewUrl = base ? (cleanPrefix ? `${base}/${cleanPrefix}` : base) : null;

  return (
    <div className={['bkt-card', isPublic && 'is-public'].filter(Boolean).join(' ')}>
      <div className="bkt-head">
        <DatabaseOutlined className="bkt-ico" />
        <span className="bkt-name" title={bucket.name}>
          {bucket.name}
        </span>
        <label
          className="bkt-public-toggle"
          title="When public, files are served via direct URLs. When private, previews use temporary signed URLs."
          style={{ display: 'inline-flex', alignItems: 'center', gap: 6, cursor: 'pointer' }}
        >
          <input
            type="checkbox"
            checked={isPublic}
            onChange={(e) => onTogglePublic(bucket.name, e.target.checked)}
            aria-label={`Public access for ${bucket.name}`}
          />
          <span className={['bkt-pill', isPublic ? 'public' : 'private'].join(' ')}>
            {isPublic ? 'Public' : 'Private'}
          </span>
        </label>
        <button
          className="fl-act-btn danger"
          onClick={() => onDelete(bucket.name)}
          title="Remove bucket"
          type="button"
        >
          <DeleteOutlined style={{ fontSize: 12 }} />
        </button>
      </div>

      {isPublic && (
        <div className="bkt-domain">
          <GlobalOutlined className="bkt-domain-ico" />
          <select
            className="select bkt-scheme"
            value={bucket.publicDomainScheme || 'https'}
            onChange={(e) => onSchemeChange(bucket.name, e.target.value)}
            aria-label={`Public domain scheme for ${bucket.name}`}
          >
            <option value="https">https://</option>
            <option value="http">http://</option>
          </select>
          <input
            className="input mono bkt-host"
            value={bucket.publicDomainHost}
            onChange={(e) => onHostChange(bucket.name, e.target.value)}
            placeholder="pub-….r2.dev or cdn.example.com"
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="off"
            spellCheck={false}
            aria-label={`Public domain for ${bucket.name}`}
          />
          <input
            className="input mono bkt-host"
            style={{ maxWidth: 140 }}
            value={bucket.publicPathPrefix}
            onChange={(e) => onPrefixChange(bucket.name, e.target.value)}
            placeholder="prefix (optional)"
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="off"
            spellCheck={false}
            aria-label={`Public path prefix for ${bucket.name}`}
          />
        </div>
      )}

      <div className={['bkt-preview', isPublic ? 'is-public' : ''].filter(Boolean).join(' ')}>
        {!isPublic ? (
          <span className="bkt-preview-hint">
            Private — files open via temporary signed URLs.
          </span>
        ) : previewUrl ? (
          <>
            <span className="bkt-preview-arrow">↳</span>
            <span className="bkt-preview-url" title={`${previewUrl}/<object>`}>
              {previewUrl}
              <span className="bkt-preview-obj">/&lt;object&gt;</span>
            </span>
          </>
        ) : isR2 ? (
          <span className="bkt-preview-hint">
            Add a public domain (r2.dev subdomain or custom domain) to serve files publicly.
          </span>
        ) : (
          <span className="bkt-preview-hint">
            Served directly from the bucket endpoint — add a custom domain above to override.
          </span>
        )}
      </div>
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
        setBuckets(
          firstToken.buckets.map((b) => ({
            name: b.name,
            isPublic: b.is_public ?? false,
            publicDomainHost: b.public_domain ?? '',
            publicDomainScheme: b.public_domain_scheme ?? 'https',
            publicPathPrefix: b.public_path_prefix ?? '',
          }))
        );
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
      setBuckets(
        acct.buckets.map((b) => ({
          name: b.name,
          isPublic: b.is_public ?? false,
          publicDomainHost: b.public_domain_host ?? '',
          publicDomainScheme: b.public_domain_scheme ?? 'https',
          publicPathPrefix: b.public_path_prefix ?? '',
        }))
      );
    } else if (acct.provider === 'minio') {
      setAccessKeyId(acct.account.access_key_id);
      setSecretAccessKey(acct.account.secret_access_key);
      setEndpointScheme(acct.account.endpoint_scheme);
      setEndpointHost(acct.account.endpoint_host);
      setBuckets(
        acct.buckets.map((b) => ({
          name: b.name,
          isPublic: b.is_public ?? false,
          publicDomainHost: b.public_domain_host ?? '',
          publicDomainScheme: b.public_domain_scheme ?? 'https',
          publicPathPrefix: b.public_path_prefix ?? '',
        }))
      );
      setRegion('');
    } else if (acct.provider === 'rustfs') {
      setAccessKeyId(acct.account.access_key_id);
      setSecretAccessKey(acct.account.secret_access_key);
      setEndpointScheme(acct.account.endpoint_scheme);
      setEndpointHost(acct.account.endpoint_host);
      setBuckets(
        acct.buckets.map((b) => ({
          name: b.name,
          isPublic: b.is_public ?? false,
          publicDomainHost: b.public_domain_host ?? '',
          publicDomainScheme: b.public_domain_scheme ?? 'https',
          publicPathPrefix: b.public_path_prefix ?? '',
        }))
      );
      setRegion('');
    }
  }, [open, accountId, accounts, isNew, initialProvider]);

  function removeBucket(name: string) {
    setBuckets((prev) => prev.filter((b) => b.name !== name));
  }

  function setBucketHost(name: string, raw: string) {
    const parsed = parseDomainInput(raw);
    setBuckets((prev) =>
      prev.map((b) =>
        b.name === name
          ? {
              ...b,
              publicDomainHost: parsed.host,
              publicDomainScheme: parsed.scheme || b.publicDomainScheme || 'https',
            }
          : b
      )
    );
  }

  function setBucketScheme(name: string, scheme: string) {
    setBuckets((prev) =>
      prev.map((b) => (b.name === name ? { ...b, publicDomainScheme: scheme } : b))
    );
  }

  function setBucketPublic(name: string, value: boolean) {
    setBuckets((prev) => prev.map((b) => (b.name === name ? { ...b, isPublic: value } : b)));
  }

  function setBucketPrefix(name: string, prefix: string) {
    setBuckets((prev) =>
      prev.map((b) => (b.name === name ? { ...b, publicPathPrefix: prefix } : b))
    );
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
      // Preserve any public-domain config already entered for known buckets.
      const merged = result.map((b) => {
        const existing = buckets.find((eb) => eb.name === b.name);
        return (
          existing ?? {
            name: b.name,
            isPublic: false,
            publicDomainHost: '',
            publicDomainScheme: 'https',
            publicPathPrefix: '',
          }
        );
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

  /** R2 stores domain in `public_domain`; S3-family stores it in `public_domain_host`. */
  function r2BucketPayload(b: BucketRow) {
    const host = b.publicDomainHost.trim();
    const prefix = b.publicPathPrefix.trim();
    return {
      name: b.name,
      public_domain: host || null,
      public_domain_scheme: host ? b.publicDomainScheme || 'https' : null,
      is_public: b.isPublic,
      public_path_prefix: prefix || null,
    };
  }
  function s3BucketPayload(b: BucketRow) {
    const host = b.publicDomainHost.trim();
    const prefix = b.publicPathPrefix.trim();
    return {
      name: b.name,
      public_domain_host: host || null,
      public_domain_scheme: host ? b.publicDomainScheme || 'https' : null,
      is_public: b.isPublic,
      public_path_prefix: prefix || null,
    };
  }

  async function handleSave() {
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
          await saveBuckets(token.id, buckets.map(r2BucketPayload));
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
          await saveAwsBuckets(acct.id, buckets.map(s3BucketPayload));
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
          await saveMinioBuckets(acct.id, buckets.map(s3BucketPayload));
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
          await saveRustfsBuckets(acct.id, buckets.map(s3BucketPayload));
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
            await saveBuckets(firstToken.token.id, buckets.map(r2BucketPayload));
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
          await saveAwsBuckets(acct.account.id, buckets.map(s3BucketPayload));
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
          await saveMinioBuckets(acct.account.id, buckets.map(s3BucketPayload));
        } else if (acct.provider === 'rustfs') {
          await updateRustfsAccount({
            id: acct.account.id,
            name: accountName.trim(),
            access_key_id: accessKeyId,
            secret_access_key: secretAccessKey,
            endpoint_scheme: endpointScheme || 'https',
            endpoint_host: endpointHost.trim(),
          });
          await saveRustfsBuckets(acct.account.id, buckets.map(s3BucketPayload));
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

  const publicCount = buckets.filter((b) => b.isPublic).length;

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
            <div className="field-label">Account name</div>
            <input
              className="input"
              value={accountName}
              onChange={(e) => setAccountName(e.target.value)}
              placeholder="My account (optional)"
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

        {/* Buckets + public access */}
        <div className="field" style={{ marginTop: 6, marginBottom: 0 }}>
          <div className="bkt-section-head">
            <div className="field-label" style={{ margin: 0 }}>
              Buckets &amp; public access
            </div>
            {buckets.length > 0 && (
              <span className="bkt-section-count">
                {publicCount > 0
                  ? `${publicCount} of ${buckets.length} public`
                  : `${buckets.length} bucket${buckets.length !== 1 ? 's' : ''}`}
              </span>
            )}
          </div>
          <div className="field-hint" style={{ marginBottom: 10 }}>
            Toggle a bucket <strong>Public</strong> to serve its files via direct URLs without
            signing, and optionally attach a custom domain (an <span className="mono">r2.dev</span>{' '}
            subdomain or your own) plus a path prefix. R2 needs a domain to be public; S3-compatible
            providers can also serve straight from their endpoint. Private buckets open via temporary
            signed links.
          </div>

          <div className="bkt-list">
            {buckets.map((b) => (
              <BucketListRow
                key={b.name}
                bucket={b}
                provider={provider}
                onDelete={removeBucket}
                onTogglePublic={setBucketPublic}
                onHostChange={setBucketHost}
                onSchemeChange={setBucketScheme}
                onPrefixChange={setBucketPrefix}
              />
            ))}
            <button
              className="btn btn-sm bkt-load-btn"
              onClick={handleLoadBuckets}
              disabled={loadingBuckets}
            >
              <ReloadOutlined spin={loadingBuckets} style={{ fontSize: 11 }} />
              {buckets.length > 0 ? 'Reload buckets from API' : 'Load buckets from API'}
            </button>
          </div>
        </div>
      </div>
    </Modal>
  );
}
