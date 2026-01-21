'use client';

import { useState, useEffect } from 'react';
import {
  Button,
  Menu,
  Drawer,
  Dropdown,
  App,
  Spin,
  Empty,
  Tooltip,
  Divider,
  Space,
  Typography,
  Tag,
} from 'antd';
import type { MenuProps } from 'antd';
import {
  UserOutlined,
  KeyOutlined,
  DatabaseOutlined,
  PlusOutlined,
  EditOutlined,
  DeleteOutlined,
  MoreOutlined,
  CheckCircleFilled,
  CloudOutlined,
  RightOutlined,
  MenuFoldOutlined,
  SwapOutlined,
} from '@ant-design/icons';
import { useAccountStore, Token, ProviderAccount } from '@/app/stores/accountStore';
import AccountTransferModal from '@/app/components/AccountTransferModal';

const { Text } = Typography;

const SIDEBAR_COLLAPSED_KEY = 'account-sidebar-collapsed';

interface AccountSidebarProps {
  onAddAccount: () => void;
  onEditAccount: (account: ProviderAccount) => void;
  onAddToken: (accountId: string) => void;
  onEditToken: (token: Token) => void;
}

export default function AccountSidebar({
  onAddAccount,
  onEditAccount,
  onAddToken,
  onEditToken,
}: AccountSidebarProps) {
  const accounts = useAccountStore((state) => state.accounts);
  const currentConfig = useAccountStore((state) => state.currentConfig);
  const loading = useAccountStore((state) => state.loading);
  const selectR2Bucket = useAccountStore((state) => state.selectR2Bucket);
  const selectAwsBucket = useAccountStore((state) => state.selectAwsBucket);
  const selectMinioBucket = useAccountStore((state) => state.selectMinioBucket);
  const selectRustfsBucket = useAccountStore((state) => state.selectRustfsBucket);
  const deleteAccount = useAccountStore((state) => state.deleteAccount);
  const deleteAwsAccount = useAccountStore((state) => state.deleteAwsAccount);
  const deleteMinioAccount = useAccountStore((state) => state.deleteMinioAccount);
  const deleteRustfsAccount = useAccountStore((state) => state.deleteRustfsAccount);
  const deleteToken = useAccountStore((state) => state.deleteToken);

  const [drawerOpen, setDrawerOpen] = useState(false);
  const [selectedAccount, setSelectedAccount] = useState<ProviderAccount | null>(null);
  const [transferModalOpen, setTransferModalOpen] = useState(false);
  const [collapsed, setCollapsed] = useState(() => {
    if (typeof window !== 'undefined') {
      const stored = localStorage.getItem(SIDEBAR_COLLAPSED_KEY);
      return stored === 'true';
    }
    return false;
  });

  const { message, modal } = App.useApp();

  // Sync selectedAccount with store when drawer is open and accounts update
  useEffect(() => {
    if (drawerOpen && selectedAccount) {
      const updatedAccount = accounts.find((a) => a.account.id === selectedAccount.account.id);
      if (updatedAccount) {
        setSelectedAccount(updatedAccount);
      }
    }
  }, [accounts, drawerOpen, selectedAccount?.account.id]);

  function toggleCollapse(value: boolean) {
    setCollapsed(value);
    localStorage.setItem(SIDEBAR_COLLAPSED_KEY, String(value));
  }

  function handleAccountClick(accountData: ProviderAccount) {
    setSelectedAccount(accountData);
    setDrawerOpen(true);
  }

  async function handleSelectBucket(
    provider: ProviderAccount['provider'],
    id: string | number,
    bucketName: string
  ) {
    try {
      if (provider === 'r2') {
        await selectR2Bucket(id as number, bucketName);
      } else if (provider === 'aws') {
        await selectAwsBucket(id as string, bucketName);
      } else if (provider === 'minio') {
        await selectMinioBucket(id as string, bucketName);
      } else {
        await selectRustfsBucket(id as string, bucketName);
      }
      message.success(`Switched to ${bucketName}`);
      setDrawerOpen(false);
    } catch {
      message.error('Failed to switch bucket');
    }
  }

  async function handleDeleteAccount(account: ProviderAccount) {
    modal.confirm({
      title: 'Delete Account',
      content:
        'Are you sure you want to delete this account? All tokens and bucket configurations will be removed.',
      okText: 'Delete',
      okButtonProps: { danger: true },
      onOk: async () => {
        try {
          if (account.provider === 'r2') {
            await deleteAccount(account.account.id);
          } else if (account.provider === 'aws') {
            await deleteAwsAccount(account.account.id);
          } else if (account.provider === 'minio') {
            await deleteMinioAccount(account.account.id);
          } else {
            await deleteRustfsAccount(account.account.id);
          }
          message.success('Account deleted');
          if (selectedAccount?.account.id === account.account.id) {
            setDrawerOpen(false);
            setSelectedAccount(null);
          }
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

  // Build menu items for accounts
  const menuItems: MenuProps['items'] = accounts.map((accountData) => {
    const account = accountData.account;
    const isCurrentAccount =
      currentConfig?.account_id === account.id && currentConfig?.provider === accountData.provider;
    const tokenCount = accountData.provider === 'r2' ? accountData.tokens.length : 0;
    const bucketCount =
      accountData.provider === 'r2'
        ? accountData.tokens.reduce((sum, t) => sum + t.buckets.length, 0)
        : accountData.buckets.length;
    const providerLabel =
      accountData.provider === 'r2'
        ? 'R2'
        : accountData.provider === 'aws'
          ? 'AWS'
          : accountData.provider === 'minio'
            ? 'MinIO'
            : 'RustFS';

    return {
      key: account.id,
      label: (
        <div className="account-menu-item">
          <UserOutlined className="account-menu-icon" />
          <div className="account-menu-content">
            <span className="account-menu-name">
              {account.name || account.id.slice(0, 12) + '...'}
            </span>
            <Tag
              color={
                accountData.provider === 'r2'
                  ? 'blue'
                  : accountData.provider === 'aws'
                    ? 'gold'
                    : accountData.provider === 'minio'
                      ? 'cyan'
                      : 'purple'
              }
              style={{ marginInlineStart: 6 }}
            >
              {providerLabel}
            </Tag>
            <Text type="secondary" className="account-menu-meta">
              {tokenCount > 0 ? `${tokenCount} token${tokenCount !== 1 ? 's' : ''}, ` : ''}
              {bucketCount} bucket{bucketCount !== 1 ? 's' : ''}
            </Text>
          </div>
          <Space size={4}>
            {isCurrentAccount && <CheckCircleFilled className="current-indicator" />}
            <Dropdown menu={{ items: getAccountContextMenu(accountData) }} trigger={['click']}>
              <Button
                type="text"
                size="small"
                icon={<MoreOutlined />}
                className="account-menu-action"
                onClick={(e) => e.stopPropagation()}
              />
            </Dropdown>
            <RightOutlined className="account-menu-arrow" />
          </Space>
        </div>
      ),
      onClick: () => handleAccountClick(accountData),
    };
  });

  if (loading) {
    return (
      <div className={`account-sidebar ${collapsed ? 'collapsed' : ''}`}>
        <div className="sidebar-header">
          <div className="sidebar-header-title" onClick={() => toggleCollapse(!collapsed)}>
            <CloudOutlined className="sidebar-header-icon" />
            {!collapsed && <span>Accounts</span>}
          </div>
          {!collapsed && (
            <Button
              type="text"
              size="small"
              icon={<MenuFoldOutlined />}
              onClick={() => toggleCollapse(true)}
            />
          )}
        </div>
        {!collapsed && (
          <div className="sidebar-loading">
            <Spin size="small" />
          </div>
        )}
      </div>
    );
  }

  return (
    <div className={`account-sidebar ${collapsed ? 'collapsed' : ''}`}>
      <div className="sidebar-header">
        <div
          className="sidebar-header-title"
          onClick={() => collapsed && toggleCollapse(false)}
          style={{ cursor: collapsed ? 'pointer' : 'default' }}
        >
          <CloudOutlined className="sidebar-header-icon" />
          {!collapsed && <span>Accounts</span>}
        </div>
        {!collapsed && (
          <Space size={4}>
            <Tooltip title="Add Account">
              <Button type="text" size="small" icon={<PlusOutlined />} onClick={onAddAccount} />
            </Tooltip>
            <Tooltip title="Collapse">
              <Button
                type="text"
                size="small"
                icon={<MenuFoldOutlined />}
                onClick={() => toggleCollapse(true)}
              />
            </Tooltip>
          </Space>
        )}
      </div>

      {collapsed ? (
        <div className="sidebar-collapsed-content">
          {accounts.map((accountData) => {
            const account = accountData.account;
            const isCurrentAccount =
              currentConfig?.account_id === account.id &&
              currentConfig?.provider === accountData.provider;
            const tokenCount = accountData.provider === 'r2' ? accountData.tokens.length : 0;
            const bucketCount =
              accountData.provider === 'r2'
                ? accountData.tokens.reduce((sum, t) => sum + t.buckets.length, 0)
                : accountData.buckets.length;
            const displayName = account.name || account.id.slice(0, 12);
            const shortName = displayName.slice(0, 3).toUpperCase();
            const providerLabel =
              accountData.provider === 'r2'
                ? 'R2'
                : accountData.provider === 'aws'
                  ? 'AWS'
                  : accountData.provider === 'minio'
                    ? 'MinIO'
                    : 'RustFS';

            return (
              <Tooltip
                key={account.id}
                title={
                  <div>
                    <div style={{ fontWeight: 500 }}>{displayName}</div>
                    <div style={{ fontSize: 12, opacity: 0.8 }}>
                      {providerLabel} ·{' '}
                      {tokenCount > 0 ? `${tokenCount} token${tokenCount !== 1 ? 's' : ''}, ` : ''}
                      {bucketCount} bucket{bucketCount !== 1 ? 's' : ''}
                    </div>
                  </div>
                }
                placement="right"
              >
                <div
                  className={`collapsed-account-item ${isCurrentAccount ? 'current' : ''}`}
                  onClick={() => handleAccountClick(accountData)}
                >
                  <span className="collapsed-account-text">{shortName}</span>
                  {isCurrentAccount && <span className="collapsed-current-dot" />}
                </div>
              </Tooltip>
            );
          })}
          <Tooltip title="Add Account" placement="right">
            <div className="collapsed-account-item add" onClick={onAddAccount}>
              <PlusOutlined />
            </div>
          </Tooltip>
        </div>
      ) : (
        <div className="sidebar-content">
          {accounts.length === 0 ? (
            <div className="sidebar-empty">
              <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="No accounts">
                <Button type="primary" icon={<PlusOutlined />} onClick={onAddAccount}>
                  Add Account
                </Button>
              </Empty>
            </div>
          ) : (
            <Menu mode="inline" items={menuItems} selectable={false} className="account-menu" />
          )}
        </div>
      )}

      <div className={`sidebar-footer${collapsed ? 'collapsed' : ''}`}>
        {collapsed ? (
          <Tooltip title="Import / Export" placement="right">
            <div className="collapsed-account-item" onClick={() => setTransferModalOpen(true)}>
              <SwapOutlined />
            </div>
          </Tooltip>
        ) : (
          <Button block icon={<SwapOutlined />} onClick={() => setTransferModalOpen(true)}>
            Import / Export
          </Button>
        )}
      </div>

      <AccountTransferModal open={transferModalOpen} onClose={() => setTransferModalOpen(false)} />

      {/* Tokens & Buckets Drawer */}
      <Drawer
        title={
          <Space>
            <UserOutlined />
            <span>
              {selectedAccount?.account.name || selectedAccount?.account.id.slice(0, 12) + '...'}
            </span>
            {selectedAccount && (
              <Tag
                color={
                  selectedAccount.provider === 'r2'
                    ? 'blue'
                    : selectedAccount.provider === 'aws'
                      ? 'gold'
                      : selectedAccount.provider === 'minio'
                        ? 'cyan'
                        : 'purple'
                }
              >
                {selectedAccount.provider === 'r2'
                  ? 'R2'
                  : selectedAccount.provider === 'aws'
                    ? 'AWS'
                    : selectedAccount.provider === 'minio'
                      ? 'MinIO'
                      : 'RustFS'}
              </Tag>
            )}
          </Space>
        }
        placement="right"
        onClose={() => setDrawerOpen(false)}
        open={drawerOpen}
        size={320}
        push={false}
        extra={
          selectedAccount?.provider === 'r2' ? (
            <Button
              type="text"
              size="small"
              icon={<PlusOutlined />}
              onClick={() => {
                if (selectedAccount) {
                  onAddToken(selectedAccount.account.id);
                }
              }}
            >
              Add Token
            </Button>
          ) : null
        }
      >
        {selectedAccount?.provider === 'r2' ? (
          selectedAccount.tokens.length === 0 ? (
            <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="No tokens">
              <Button
                type="primary"
                icon={<PlusOutlined />}
                onClick={() => onAddToken(selectedAccount.account.id)}
              >
                Add Token
              </Button>
            </Empty>
          ) : (
            selectedAccount.tokens.map((tokenData, index) => {
              const token = tokenData.token;
              const isCurrentToken =
                currentConfig?.provider === 'r2' && currentConfig?.token_id === token.id;

              return (
                <div key={token.id}>
                  {index > 0 && <Divider style={{ margin: '16px 0' }} />}

                  <div className="drawer-token-header">
                    <div className="drawer-token-info">
                      <KeyOutlined className="drawer-token-icon" />
                      <span className="drawer-token-name">{token.name || 'Unnamed Token'}</span>
                      {isCurrentToken && <CheckCircleFilled className="current-indicator" />}
                    </div>
                    <Dropdown menu={{ items: getTokenContextMenu(token) }} trigger={['click']}>
                      <Button type="text" size="small" icon={<MoreOutlined />} />
                    </Dropdown>
                  </div>

                  <div className="drawer-buckets">
                    {tokenData.buckets.map((bucket) => {
                      const isCurrentBucket =
                        isCurrentToken && currentConfig?.bucket === bucket.name;

                      return (
                        <Tooltip
                          key={bucket.name}
                          title={
                            bucket.public_domain
                              ? `${bucket.public_domain_scheme || 'https'}://${bucket.public_domain}`
                              : null
                          }
                          placement="right"
                        >
                          <div
                            className={`drawer-bucket-item ${isCurrentBucket ? 'current' : ''}`}
                            onClick={() => handleSelectBucket('r2', token.id, bucket.name)}
                          >
                            <DatabaseOutlined className="drawer-bucket-icon" />
                            <span className="drawer-bucket-name">{bucket.name}</span>
                            {isCurrentBucket && (
                              <CheckCircleFilled className="current-indicator active" />
                            )}
                          </div>
                        </Tooltip>
                      );
                    })}
                    {tokenData.buckets.length === 0 && (
                      <Text type="secondary" style={{ padding: '8px 12px', display: 'block' }}>
                        No buckets configured
                      </Text>
                    )}
                  </div>
                </div>
              );
            })
          )
        ) : selectedAccount ? (
          <div className="drawer-buckets">
            {selectedAccount.buckets.map((bucket) => {
              const isCurrentBucket =
                currentConfig?.provider === selectedAccount.provider &&
                currentConfig?.account_id === selectedAccount.account.id &&
                currentConfig?.bucket === bucket.name;

              return (
                <Tooltip
                  key={bucket.name}
                  title={
                    bucket.public_domain_host
                      ? `${bucket.public_domain_scheme || 'https'}://${bucket.public_domain_host}`
                      : null
                  }
                  placement="right"
                >
                  <div
                    className={`drawer-bucket-item ${isCurrentBucket ? 'current' : ''}`}
                    onClick={() =>
                      handleSelectBucket(
                        selectedAccount.provider,
                        selectedAccount.account.id,
                        bucket.name
                      )
                    }
                  >
                    <DatabaseOutlined className="drawer-bucket-icon" />
                    <span className="drawer-bucket-name">{bucket.name}</span>
                    {isCurrentBucket && <CheckCircleFilled className="current-indicator active" />}
                  </div>
                </Tooltip>
              );
            })}
            {selectedAccount.buckets.length === 0 && (
              <Text type="secondary" style={{ padding: '8px 12px', display: 'block' }}>
                No buckets configured
              </Text>
            )}
          </div>
        ) : null}
      </Drawer>
    </div>
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
} from '../stores/accountStore';
