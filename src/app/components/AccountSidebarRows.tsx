'use client';

/**
 * Sub-components for AccountSidebar rows.
 * Kept in a separate file to stay within 600-line limit per file.
 */

import { Dropdown, Tooltip } from 'antd';
import type { MenuProps } from 'antd';
import { DatabaseOutlined, MoreOutlined, RightOutlined } from '@ant-design/icons';
import {
  Token,
  ProviderAccount,
  Bucket,
  AwsBucket,
  MinioBucket,
  RustfsBucket,
} from '@/app/stores/accountStore';

// ── Provider icon ─────────────────────────────────────────────────

type Provider = ProviderAccount['provider'];

const PROVIDER_CONFIG: Record<Provider, { cls: string; label: string }> = {
  r2: { cls: 'pi-r2', label: 'R2' },
  aws: { cls: 'pi-aws', label: 'S3' },
  minio: { cls: 'pi-minio', label: 'M' },
  rustfs: { cls: 'pi-rustfs', label: 'RF' },
};

export function ProviderIcon({ provider }: { provider: Provider }) {
  const { cls, label } = PROVIDER_CONFIG[provider];
  return <span className={`sb-provider-icon ${cls}`}>{label}</span>;
}

// ── Bucket row ────────────────────────────────────────────────────

interface BucketRowProps {
  bucketName: string;
  active: boolean;
  size?: string | null;
  onClick: () => void;
}

export function BucketRow({ bucketName, active, size, onClick }: BucketRowProps) {
  return (
    <div
      className={['sb-bucket-row', active ? 'active' : ''].filter(Boolean).join(' ')}
      onClick={onClick}
    >
      <DatabaseOutlined className="sb-bucket-icon" style={{ fontSize: 12 }} />
      <span className="sb-bucket-name">{bucketName}</span>
      {size ? <span className="sb-bucket-size">{size}</span> : null}
    </div>
  );
}

// ── R2 account children (tokens → buckets) ────────────────────────

interface R2AccountChildrenProps {
  accountData: ProviderAccount & { provider: 'r2' };
  currentTokenId: number | null | undefined;
  currentBucket: string | undefined;
  search: string;
  onSelectBucket: (tokenId: number, bucketName: string) => void;
  getTokenContextMenu: (token: Token) => MenuProps['items'];
}

export function R2AccountChildren({
  accountData,
  currentTokenId,
  currentBucket,
  search,
  onSelectBucket,
  getTokenContextMenu,
}: R2AccountChildrenProps) {
  const tokens = accountData.tokens;

  const filteredTokens = search
    ? tokens
        .map((td) => ({
          ...td,
          buckets: td.buckets.filter(
            (b) =>
              b.name.toLowerCase().includes(search) ||
              accountData.account.name?.toLowerCase().includes(search) ||
              accountData.account.id.toLowerCase().includes(search)
          ),
        }))
        .filter(
          (td) =>
            td.buckets.length > 0 ||
            td.token.name?.toLowerCase().includes(search) ||
            accountData.account.name?.toLowerCase().includes(search)
        )
    : tokens;

  if (filteredTokens.length === 0) {
    return (
      <div className="sb-children">
        <div style={{ padding: '6px 10px', fontSize: 11, color: 'var(--text-subtle)' }}>
          No tokens yet
        </div>
      </div>
    );
  }

  return (
    <div className="sb-children">
      {filteredTokens.map((tokenData) => {
        const token = tokenData.token;
        return (
          <div key={token.id}>
            <div
              style={{
                display: 'flex',
                alignItems: 'center',
                justifyContent: 'space-between',
                padding: '6px 6px 4px 10px',
              }}
            >
              <span
                style={{
                  fontSize: 10.5,
                  color: 'var(--text-subtle)',
                  fontFamily: 'var(--font-mono)',
                  overflow: 'hidden',
                  textOverflow: 'ellipsis',
                  whiteSpace: 'nowrap',
                  flex: 1,
                  minWidth: 0,
                }}
              >
                ↳ {token.name || 'Unnamed Token'}
              </span>
              <Dropdown menu={{ items: getTokenContextMenu(token) }} trigger={['click']}>
                <button
                  className="sb-icon-btn"
                  onClick={(e) => e.stopPropagation()}
                  title="Token options"
                  style={{ width: 20, height: 20, flexShrink: 0 }}
                >
                  <MoreOutlined style={{ fontSize: 11 }} />
                </button>
              </Dropdown>
            </div>
            {tokenData.buckets.length === 0 ? (
              <div style={{ padding: '4px 10px 6px', fontSize: 11, color: 'var(--text-subtle)' }}>
                No buckets
              </div>
            ) : (
              tokenData.buckets.map((bucket: Bucket) => (
                <BucketRow
                  key={bucket.name}
                  bucketName={bucket.name}
                  active={currentTokenId === token.id && currentBucket === bucket.name}
                  onClick={() => onSelectBucket(token.id, bucket.name)}
                />
              ))
            )}
          </div>
        );
      })}
    </div>
  );
}

// ── Non-R2 account children (buckets directly) ────────────────────

type GenericBucket = AwsBucket | MinioBucket | RustfsBucket;

interface NonR2AccountChildrenProps {
  accountData: ProviderAccount & { provider: 'aws' | 'minio' | 'rustfs' };
  currentBucket: string | undefined;
  isCurrentAccount: boolean;
  search: string;
  onSelectBucket: (bucketName: string) => void;
}

export function NonR2AccountChildren({
  accountData,
  currentBucket,
  isCurrentAccount,
  search,
  onSelectBucket,
}: NonR2AccountChildrenProps) {
  const buckets = (accountData.buckets as GenericBucket[]).filter((b) =>
    search
      ? b.name.toLowerCase().includes(search) ||
        accountData.account.name?.toLowerCase().includes(search) ||
        accountData.account.id.toLowerCase().includes(search)
      : true
  );

  if (buckets.length === 0) {
    return (
      <div className="sb-children">
        <div style={{ padding: '6px 10px', fontSize: 11, color: 'var(--text-subtle)' }}>
          No buckets
        </div>
      </div>
    );
  }

  return (
    <div className="sb-children">
      {buckets.map((bucket) => (
        <BucketRow
          key={bucket.name}
          bucketName={bucket.name}
          active={isCurrentAccount && currentBucket === bucket.name}
          onClick={() => onSelectBucket(bucket.name)}
        />
      ))}
    </div>
  );
}

// ── Account row ───────────────────────────────────────────────────

interface AccountRowProps {
  accountData: ProviderAccount;
  expanded: boolean;
  collapsed: boolean;
  onToggle: () => void;
  contextMenu: MenuProps['items'];
}

export function AccountRow({
  accountData,
  expanded,
  collapsed,
  onToggle,
  contextMenu,
}: AccountRowProps) {
  const account = accountData.account;
  const displayName = account.name || account.id.slice(0, 12) + '…';
  const bucketCount =
    accountData.provider === 'r2'
      ? accountData.tokens.reduce((n, t) => n + t.buckets.length, 0)
      : accountData.buckets.length;

  const row = (
    <div className="sb-account-row" onClick={onToggle}>
      {!collapsed && (
        <span className={['sb-chev', expanded ? 'open' : ''].filter(Boolean).join(' ')}>
          <RightOutlined style={{ fontSize: 9 }} />
        </span>
      )}
      <ProviderIcon provider={accountData.provider} />
      {!collapsed && (
        <>
          <span className="sb-account-name">{displayName}</span>
          <span className="sb-account-meta">{bucketCount}</span>
        </>
      )}
      {!collapsed && (
        <Dropdown menu={{ items: contextMenu }} trigger={['click']}>
          <button
            className="sb-icon-btn"
            onClick={(e) => e.stopPropagation()}
            title="Account options"
            style={{ width: 20, height: 20, flexShrink: 0 }}
          >
            <MoreOutlined style={{ fontSize: 11 }} />
          </button>
        </Dropdown>
      )}
    </div>
  );

  if (collapsed) {
    return (
      <Tooltip title={displayName} placement="right">
        {row}
      </Tooltip>
    );
  }

  return row;
}
