'use client';

import { useState, useMemo } from 'react';
import { Dropdown, App } from 'antd';
import type { MenuProps } from 'antd';
import {
  PlusOutlined,
  EditOutlined,
  DeleteOutlined,
  SettingOutlined,
  MenuFoldOutlined,
  SearchOutlined,
  SwapOutlined,
} from '@ant-design/icons';
import { useAccountStore, Token, ProviderAccount } from '@/app/stores/accountStore';
import { useThemeStore } from '@/app/stores/themeStore';
import { useCurrentPathStore } from '@/app/stores/currentPathStore';
import AccountTransferModal from '@/app/components/AccountTransferModal';
import {
  AccountRow,
  R2AccountChildren,
  NonR2AccountChildren,
} from '@/app/components/AccountSidebarRows';

interface AccountSidebarProps {
  onAddAccount: () => void;
  onEditAccount: (account: ProviderAccount) => void;
  onAddToken: (accountId: string) => void;
  onEditToken: (token: Token) => void;
  onOpenSettings?: () => void;
}

export default function AccountSidebar({
  onAddAccount,
  onEditAccount,
  onAddToken,
  onEditToken,
  onOpenSettings,
}: AccountSidebarProps) {
  const accounts = useAccountStore((state) => state.accounts);
  const currentConfig = useAccountStore((state) => state.currentConfig);
  const selectR2Bucket = useAccountStore((state) => state.selectR2Bucket);
  const selectAwsBucket = useAccountStore((state) => state.selectAwsBucket);
  const selectMinioBucket = useAccountStore((state) => state.selectMinioBucket);
  const selectRustfsBucket = useAccountStore((state) => state.selectRustfsBucket);
  const deleteAccount = useAccountStore((state) => state.deleteAccount);
  const deleteAwsAccount = useAccountStore((state) => state.deleteAwsAccount);
  const deleteMinioAccount = useAccountStore((state) => state.deleteMinioAccount);
  const deleteRustfsAccount = useAccountStore((state) => state.deleteRustfsAccount);
  const deleteToken = useAccountStore((state) => state.deleteToken);

  const sidebarStyle = useThemeStore((state) => state.sidebarStyle);
  const cycleSidebarStyle = useThemeStore((state) => state.cycleSidebarStyle);
  const setSidebarStyle = useThemeStore((state) => state.setSidebarStyle);
  const setCurrentPath = useCurrentPathStore((state) => state.setCurrentPath);

  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [search, setSearch] = useState('');
  const [transferModalOpen, setTransferModalOpen] = useState(false);

  const { message, modal } = App.useApp();

  const collapsed = sidebarStyle === 'collapsed';

  const sidebarCls = [
    'sidebar',
    sidebarStyle === 'collapsed' ? 'collapsed-sidebar' : '',
    sidebarStyle === 'floating' ? 'floating-sidebar' : '',
  ]
    .filter(Boolean)
    .join(' ');

  const searchLower = search.trim().toLowerCase();

  const filteredAccounts = useMemo(() => {
    if (!searchLower) return accounts;
    return accounts.filter((a) => {
      const nameMatch =
        a.account.name?.toLowerCase().includes(searchLower) ||
        a.account.id.toLowerCase().includes(searchLower);
      if (nameMatch) return true;
      if (a.provider === 'r2') {
        return a.tokens.some(
          (td) =>
            td.token.name?.toLowerCase().includes(searchLower) ||
            td.buckets.some((b) => b.name.toLowerCase().includes(searchLower))
        );
      }
      return a.buckets.some((b) => b.name.toLowerCase().includes(searchLower));
    });
  }, [accounts, searchLower]);

  function toggleExpanded(id: string) {
    // Clicking an account icon while collapsed expands the sidebar and opens that account.
    if (sidebarStyle === 'collapsed') {
      setSidebarStyle('full');
      setExpanded((prev) => ({ ...prev, [id]: true }));
      return;
    }
    setExpanded((prev) => ({ ...prev, [id]: !prev[id] }));
  }

  async function handleSelectR2Bucket(tokenId: number, bucketName: string) {
    try {
      setCurrentPath('');
      await selectR2Bucket(tokenId, bucketName);
    } catch {
      message.error('Failed to switch bucket');
    }
  }

  async function handleSelectNonR2Bucket(
    accountData: ProviderAccount & { provider: 'aws' | 'minio' | 'rustfs' },
    bucketName: string
  ) {
    try {
      setCurrentPath('');
      if (accountData.provider === 'aws') {
        await selectAwsBucket(accountData.account.id, bucketName);
      } else if (accountData.provider === 'minio') {
        await selectMinioBucket(accountData.account.id, bucketName);
      } else {
        await selectRustfsBucket(accountData.account.id, bucketName);
      }
    } catch {
      message.error('Failed to switch bucket');
    }
  }

  async function handleDeleteAccount(accountData: ProviderAccount) {
    modal.confirm({
      title: 'Delete Account',
      content:
        'Are you sure you want to delete this account? All tokens and bucket configurations will be removed.',
      okText: 'Delete',
      okButtonProps: { danger: true },
      onOk: async () => {
        try {
          if (accountData.provider === 'r2') {
            await deleteAccount(accountData.account.id);
          } else if (accountData.provider === 'aws') {
            await deleteAwsAccount(accountData.account.id);
          } else if (accountData.provider === 'minio') {
            await deleteMinioAccount(accountData.account.id);
          } else {
            await deleteRustfsAccount(accountData.account.id);
          }
          message.success('Account deleted');
        } catch {
          message.error('Failed to delete account');
        }
      },
    });
  }

  async function handleDeleteToken(tokenId: number) {
    modal.confirm({
      title: 'Delete Token',
      content:
        'Are you sure you want to delete this token? All bucket configurations will be removed.',
      okText: 'Delete',
      okButtonProps: { danger: true },
      onOk: async () => {
        try {
          await deleteToken(tokenId);
          message.success('Token deleted');
        } catch {
          message.error('Failed to delete token');
        }
      },
    });
  }

  function getAccountContextMenu(accountData: ProviderAccount): MenuProps['items'] {
    const items: MenuProps['items'] = [
      {
        key: 'edit',
        label: 'Edit Account',
        icon: <EditOutlined />,
        onClick: (e) => {
          e.domEvent.stopPropagation();
          onEditAccount(accountData);
        },
      },
    ];

    if (accountData.provider === 'r2') {
      items.push({
        key: 'add-token',
        label: 'Add Token',
        icon: <PlusOutlined />,
        onClick: (e) => {
          e.domEvent.stopPropagation();
          onAddToken(accountData.account.id);
        },
      });
    }

    items.push({
      key: 'transfer',
      label: 'Transfer accounts…',
      icon: <SwapOutlined />,
      onClick: (e) => {
        e.domEvent.stopPropagation();
        setTransferModalOpen(true);
      },
    });

    items.push({ type: 'divider' });
    items.push({
      key: 'delete',
      label: 'Delete Account',
      icon: <DeleteOutlined />,
      danger: true,
      onClick: (e) => {
        e.domEvent.stopPropagation();
        handleDeleteAccount(accountData);
      },
    });

    return items;
  }

  function getTokenContextMenu(token: Token): MenuProps['items'] {
    return [
      {
        key: 'edit',
        label: 'Edit Token',
        icon: <EditOutlined />,
        onClick: () => onEditToken(token),
      },
      { type: 'divider' },
      {
        key: 'delete',
        label: 'Delete Token',
        icon: <DeleteOutlined />,
        danger: true,
        onClick: () => handleDeleteToken(token.id),
      },
    ];
  }

  return (
    <>
      <aside className={sidebarCls}>
        {/* Standalone collapse toggle row, shown only when collapsed —
            sits between the brand and the account list */}
        {collapsed && (
          <div className="sb-collapse-rail">
            <button
              className="sb-icon-btn"
              title="Cycle sidebar style"
              onClick={cycleSidebarStyle}
            >
              <MenuFoldOutlined style={{ fontSize: 14 }} />
            </button>
          </div>
        )}

        {/* Search + collapse toggle, single row in full mode */}
        {!collapsed && (
          <div className="sb-search-row">
            <div className="sb-search">
              <SearchOutlined className="search-icon" style={{ fontSize: 13 }} />
              <input
                placeholder="Filter accounts & buckets"
                value={search}
                onChange={(e) => setSearch(e.target.value)}
              />
            </div>
            <button
              className="sb-icon-btn"
              title="Cycle sidebar style"
              onClick={cycleSidebarStyle}
            >
              <MenuFoldOutlined style={{ fontSize: 14 }} />
            </button>
          </div>
        )}

        {/* Section label */}
        {!collapsed && <div className="sb-section-label">Accounts</div>}

        {/* Account tree */}
        <div className="sb-scroll">
          {filteredAccounts.map((accountData) => {
            const id = accountData.account.id;
            const isExpanded = !!expanded[id];
            const isCurrentAccount =
              currentConfig?.account_id === id && currentConfig?.provider === accountData.provider;

            return (
              <div className="sb-account" key={id}>
                <AccountRow
                  accountData={accountData}
                  expanded={isExpanded}
                  collapsed={collapsed}
                  onToggle={() => toggleExpanded(id)}
                  contextMenu={getAccountContextMenu(accountData)}
                />

                {isExpanded && !collapsed && accountData.provider === 'r2' && (
                  <R2AccountChildren
                    accountData={accountData}
                    currentTokenId={currentConfig?.token_id}
                    currentBucket={currentConfig?.bucket}
                    search={searchLower}
                    onSelectBucket={handleSelectR2Bucket}
                    getTokenContextMenu={getTokenContextMenu}
                  />
                )}

                {isExpanded && !collapsed && accountData.provider !== 'r2' && (
                  <NonR2AccountChildren
                    accountData={
                      accountData as ProviderAccount & {
                        provider: 'aws' | 'minio' | 'rustfs';
                      }
                    }
                    currentBucket={currentConfig?.bucket}
                    isCurrentAccount={isCurrentAccount}
                    search={searchLower}
                    onSelectBucket={(bucketName) =>
                      handleSelectNonR2Bucket(
                        accountData as ProviderAccount & {
                          provider: 'aws' | 'minio' | 'rustfs';
                        },
                        bucketName
                      )
                    }
                  />
                )}
              </div>
            );
          })}

          {filteredAccounts.length === 0 && !collapsed && (
            <div
              style={{
                padding: '16px 12px',
                fontSize: 12,
                color: 'var(--text-subtle)',
                textAlign: 'center',
              }}
            >
              {searchLower ? 'No matching accounts' : 'No accounts yet'}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="sb-footer">
          <button className="sb-footer-btn" onClick={onAddAccount}>
            <PlusOutlined style={{ fontSize: 11 }} />
            {!collapsed && <span>Add account</span>}
          </button>
          {!collapsed && (
            <button
              className="sb-icon-btn"
              onClick={onOpenSettings ?? (() => undefined)}
              title="Settings"
              style={{ width: 30, height: 30 }}
            >
              <SettingOutlined style={{ fontSize: 14 }} />
            </button>
          )}
        </div>
      </aside>

      {transferModalOpen && (
        <AccountTransferModal open={true} onClose={() => setTransferModalOpen(false)} />
      )}
    </>
  );
}

// Re-export types from store for convenience
export type {
  Account,
  Token,
  Bucket,
  TokenWithBuckets,
  AccountWithTokens,
  CurrentConfig,
  AwsAccount,
  MinioAccount,
  AwsBucket,
  MinioBucket,
  ProviderAccount,
} from '@/app/stores/accountStore';
