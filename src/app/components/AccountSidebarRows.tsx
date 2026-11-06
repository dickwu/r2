'use client';

/**
 * Sub-components for AccountSidebar rows.
 * Kept in a separate file to stay within 600-line limit per file.
 */

import { useTranslation } from 'react-i18next';
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
  contextMenu?: MenuProps['items'];
  /** Set when the bucket is mounted locally — drives the marker and its tooltip. */
  mountedPath?: string | null;
  /** Makes the mounted marker a shortcut into Settings › Mounts. */
  onOpenMounts?: () => void;
}

export function BucketRow({
  bucketName,
  active,
  size,
  onClick,
  contextMenu,
  mountedPath,
  onOpenMounts,
}: BucketRowProps) {
  const { t } = useTranslation();
  return (
    <div
      className={['sb-bucket-row', active ? 'active' : ''].filter(Boolean).join(' ')}
      onClick={onClick}
    >
      <DatabaseOutlined className="sb-bucket-icon" style={{ fontSize: 12 }} />
      <span className="sb-bucket-name">{bucketName}</span>
      {mountedPath ? (
        onOpenMounts ? (
          <Tooltip title={t('sidebar.mountedManage', { path: mountedPath })} placement="right">
            <button
              className="sb-bucket-mounted-btn"
              onClick={(e) => {
                e.stopPropagation();
                onOpenMounts();
              }}
              aria-label={t('sidebar.mountedManageAria', { path: mountedPath })}
            >
              <span className="sb-bucket-mounted" />
            </button>
          </Tooltip>
        ) : (
          <Tooltip title={t('sidebar.mountedAt', { path: mountedPath })} placement="right">
            <span
              className="sb-bucket-mounted"
              role="img"
              aria-label={t('sidebar.mountedAt', { path: mountedPath })}
            />
          </Tooltip>
        )
      ) : null}
      {size ? <span className="sb-bucket-size">{size}</span> : null}
      {contextMenu ? (
        <Dropdown menu={{ items: contextMenu }} trigger={['click']}>
          <button
            className="sb-icon-btn"
            onClick={(e) => e.stopPropagation()}
            title={t('sidebar.bucketOptions')}
            style={{ width: 20, height: 20, flexShrink: 0 }}
          >
            <MoreOutlined style={{ fontSize: 11 }} />
          </button>
        </Dropdown>
      ) : null}
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
  getBucketContextMenu: (token: Token, bucketName: string) => MenuProps['items'];
  getMountedPath: (bucketName: string) => string | null;
  onOpenMounts?: () => void;
}

export function R2AccountChildren({
  accountData,
  currentTokenId,
  currentBucket,
  search,
  onSelectBucket,
  getTokenContextMenu,
  getBucketContextMenu,
  getMountedPath,
  onOpenMounts,
}: R2AccountChildrenProps) {
  const { t } = useTranslation();
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
          {t('sidebar.noTokens')}
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
                ↳ {token.name || t('sidebar.unnamedToken')}
              </span>
              <Dropdown menu={{ items: getTokenContextMenu(token) }} trigger={['click']}>
                <button
                  className="sb-icon-btn"
                  onClick={(e) => e.stopPropagation()}
                  title={t('sidebar.tokenOptions')}
                  style={{ width: 20, height: 20, flexShrink: 0 }}
                >
                  <MoreOutlined style={{ fontSize: 11 }} />
                </button>
              </Dropdown>
            </div>
            {tokenData.buckets.length === 0 ? (
              <div style={{ padding: '4px 10px 6px', fontSize: 11, color: 'var(--text-subtle)' }}>
                {t('sidebar.noBuckets')}
              </div>
            ) : (
              tokenData.buckets.map((bucket: Bucket) => (
                <BucketRow
                  key={bucket.name}
                  bucketName={bucket.name}
                  active={currentTokenId === token.id && currentBucket === bucket.name}
                  onClick={() => onSelectBucket(token.id, bucket.name)}
                  contextMenu={getBucketContextMenu(token, bucket.name)}
                  mountedPath={getMountedPath(bucket.name)}
                  onOpenMounts={onOpenMounts}
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
  getBucketContextMenu: (bucketName: string) => MenuProps['items'];
  getMountedPath: (bucketName: string) => string | null;
  onOpenMounts?: () => void;
}

export function NonR2AccountChildren({
  accountData,
  currentBucket,
  isCurrentAccount,
  search,
  onSelectBucket,
  getBucketContextMenu,
  getMountedPath,
  onOpenMounts,
}: NonR2AccountChildrenProps) {
  const { t } = useTranslation();
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
          {t('sidebar.noBuckets')}
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
          contextMenu={getBucketContextMenu(bucket.name)}
          mountedPath={getMountedPath(bucket.name)}
          onOpenMounts={onOpenMounts}
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
  const { t } = useTranslation();
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
            title={t('sidebar.accountOptions')}
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
