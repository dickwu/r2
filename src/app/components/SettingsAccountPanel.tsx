'use client';

import { useState } from 'react';
import { PlusOutlined, DatabaseOutlined, RightOutlined } from '@ant-design/icons';
import { useAccountStore, type ProviderAccount } from '@/app/stores/accountStore';
import type { StorageProvider } from '@/app/lib/r2cache';
import AccountEditModal from '@/app/components/AccountEditModal';

export interface SettingsAccountPanelProps {
  /** When provided, immediately open the edit modal for this account on mount. */
  initialAccountId?: string;
}

/* ── Provider badge metadata (mirrors AccountEditModal) ─────────── */
const PROVIDER_META: Record<StorageProvider, { label: string; icon: string }> = {
  r2: { label: 'Cloudflare R2', icon: 'R2' },
  aws: { label: 'AWS S3', icon: 'S3' },
  minio: { label: 'MinIO', icon: 'M' },
  rustfs: { label: 'RustFS', icon: 'RF' },
};

/* ── Bucket count helper ─────────────────────────────────────────── */
function bucketCount(a: ProviderAccount): number {
  if (a.provider === 'r2') {
    return a.tokens.reduce((sum, t) => sum + t.buckets.length, 0);
  }
  return a.buckets.length;
}

/* ── AccountCard ─────────────────────────────────────────────────── */
function AccountCard({
  accountData,
  onClick,
}: {
  accountData: ProviderAccount;
  onClick: () => void;
}) {
  const meta = PROVIDER_META[accountData.provider];
  const name = accountData.account.name ?? accountData.account.id;
  const count = bucketCount(accountData);

  return (
    <button className="settings-account-card" onClick={onClick}>
      <div className={`pi pi-${accountData.provider}`}>{meta.icon}</div>
      <div className="settings-account-card-body">
        <div className="settings-account-card-name">{name}</div>
        <div className="settings-account-card-meta">
          <span>{meta.label}</span>
          <span className="settings-account-card-dot" />
          <span>
            <DatabaseOutlined style={{ fontSize: 10, marginRight: 4 }} />
            {count} {count === 1 ? 'bucket' : 'buckets'}
          </span>
        </div>
      </div>
      <RightOutlined style={{ fontSize: 11, color: 'var(--text-subtle)' }} />
    </button>
  );
}

/* ── SettingsAccountPanel ────────────────────────────────────────── */
export default function SettingsAccountPanel({ initialAccountId }: SettingsAccountPanelProps) {
  const accounts = useAccountStore((s) => s.accounts);

  // Edit-modal state
  // - editing === undefined ⇒ modal closed
  // - editing === null      ⇒ modal open in "new account" mode
  // - editing === <id>      ⇒ modal open in "edit existing" mode
  const [editing, setEditing] = useState<string | null | undefined>(
    initialAccountId ? initialAccountId : undefined
  );

  return (
    <>
      <div className="settings-section-stack">
        <section className="settings-section">
          <div className="settings-section-head">
            <div>
              <h3>Connected accounts</h3>
              <p>Click an account to edit credentials, buckets, or remove it.</p>
            </div>
            <button className="btn btn-primary btn-sm" onClick={() => setEditing(null)}>
              <PlusOutlined style={{ fontSize: 11 }} />
              New account
            </button>
          </div>

          {accounts.length === 0 ? (
            <div className="settings-account-empty">
              <DatabaseOutlined style={{ fontSize: 22, color: 'var(--text-subtle)' }} />
              <strong>No accounts yet</strong>
              <span>Add your first storage provider to get started.</span>
              <button
                className="btn btn-primary"
                style={{ marginTop: 10 }}
                onClick={() => setEditing(null)}
              >
                <PlusOutlined style={{ fontSize: 11 }} />
                Add account
              </button>
            </div>
          ) : (
            <div className="settings-account-list">
              {accounts.map((a) => (
                <AccountCard
                  key={a.account.id}
                  accountData={a}
                  onClick={() => setEditing(a.account.id)}
                />
              ))}
            </div>
          )}
        </section>
      </div>

      <AccountEditModal
        open={editing !== undefined}
        accountId={editing ?? null}
        onClose={() => setEditing(undefined)}
      />
    </>
  );
}
