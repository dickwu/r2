'use client';

import { useState } from 'react';
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
} from '@ant-design/icons';
import { useAccountStore, Account, Token, AccountWithTokens } from '../stores/accountStore';

const { Text } = Typography;

const SIDEBAR_COLLAPSED_KEY = 'account-sidebar-collapsed';

interface AccountSidebarProps {
  onAddAccount: () => void;
  onEditAccount: (account: Account) => void;
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
  const selectBucket = useAccountStore((state) => state.selectBucket);
  const deleteAccount = useAccountStore((state) => state.deleteAccount);
  const deleteToken = useAccountStore((state) => state.deleteToken);

  const [drawerOpen, setDrawerOpen] = useState(false);
  const [selectedAccount, setSelectedAccount] = useState<AccountWithTokens | null>(null);
  const [collapsed, setCollapsed] = useState(() => {
    if (typeof window !== 'undefined') {
      const stored = localStorage.getItem(SIDEBAR_COLLAPSED_KEY);
      return stored === 'true';
    }
    return false;
  });

  const { message, modal } = App.useApp();

  function toggleCollapse(value: boolean) {
    setCollapsed(value);
    localStorage.setItem(SIDEBAR_COLLAPSED_KEY, String(value));
  }

  function handleAccountClick(accountData: AccountWithTokens) {
    setSelectedAccount(accountData);
    setDrawerOpen(true);
  }

  async function handleSelectBucket(tokenId: number, bucketName: string) {
    try {
      await selectBucket(tokenId, bucketName);
      message.success(`Switched to ${bucketName}`);
      setDrawerOpen(false);
    } catch {
      message.error('Failed to switch bucket');
    }
  }

  async function handleDeleteAccount(accountId: string) {
    modal.confirm({
      title: 'Delete Account',
      content:
        'Are you sure you want to delete this account? All tokens and bucket configurations will be removed.',
      okText: 'Delete',
      okButtonProps: { danger: true },
      onOk: async () => {
        try {
          await deleteAccount(accountId);
          message.success('Account deleted');
          if (selectedAccount?.account.id === accountId) {
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

  function getAccountContextMenu(account: Account): MenuProps['items'] {
    return [
      {
        key: 'edit',
        label: 'Edit Account',
        icon: <EditOutlined />,
        onClick: (e) => {
          e.domEvent.stopPropagation();
          onEditAccount(account);
        },
      },
      {
        key: 'add-token',
        label: 'Add Token',
        icon: <PlusOutlined />,
        onClick: (e) => {
          e.domEvent.stopPropagation();
          onAddToken(account.id);
        },
      },
      { type: 'divider' },
      {
        key: 'delete',
        label: 'Delete Account',
        icon: <DeleteOutlined />,
        danger: true,
        onClick: (e) => {
          e.domEvent.stopPropagation();
          handleDeleteAccount(account.id);
        },
      },
    ];
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
    const isCurrentAccount = currentConfig?.account_id === account.id;
    const tokenCount = accountData.tokens.length;
    const bucketCount = accountData.tokens.reduce((sum, t) => sum + t.buckets.length, 0);

    return {
      key: account.id,
      label: (
        <div className="account-menu-item">
          <UserOutlined className="account-menu-icon" />
          <div className="account-menu-content">
            <span className="account-menu-name">
              {account.name || account.id.slice(0, 12) + '...'}
            </span>
            <Text type="secondary" className="account-menu-meta">
              {tokenCount} token{tokenCount !== 1 ? 's' : ''}, {bucketCount} bucket
              {bucketCount !== 1 ? 's' : ''}
            </Text>
          </div>
          <Space size={4}>
            {isCurrentAccount && <CheckCircleFilled className="current-indicator" />}
            <Dropdown menu={{ items: getAccountContextMenu(account) }} trigger={['click']}>
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
            const isCurrentAccount = currentConfig?.account_id === account.id;
            const tokenCount = accountData.tokens.length;
            const bucketCount = accountData.tokens.reduce((sum, t) => sum + t.buckets.length, 0);
            const displayName = account.name || account.id.slice(0, 12);
            const shortName = displayName.slice(0, 3).toUpperCase();

            return (
              <Tooltip
                key={account.id}
                title={
                  <div>
                    <div style={{ fontWeight: 500 }}>{displayName}</div>
                    <div style={{ fontSize: 12, opacity: 0.8 }}>
                      {tokenCount} token{tokenCount !== 1 ? 's' : ''}, {bucketCount} bucket
                      {bucketCount !== 1 ? 's' : ''}
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

      {/* Tokens & Buckets Drawer */}
      <Drawer
        title={
          <Space>
            <UserOutlined />
            <span>
              {selectedAccount?.account.name || selectedAccount?.account.id.slice(0, 12) + '...'}
            </span>
          </Space>
        }
        placement="right"
        onClose={() => setDrawerOpen(false)}
        open={drawerOpen}
        size={320}
        push={false}
        extra={
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
        }
      >
        {selectedAccount?.tokens.length === 0 ? (
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
          selectedAccount?.tokens.map((tokenData, index) => {
            const token = tokenData.token;
            const isCurrentToken = currentConfig?.token_id === token.id;

            return (
              <div key={token.id}>
                {index > 0 && <Divider style={{ margin: '16px 0' }} />}

                {/* Token Header */}
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

                {/* Buckets */}
                <div className="drawer-buckets">
                  {tokenData.buckets.map((bucket) => {
                    const isCurrentBucket = isCurrentToken && currentConfig?.bucket === bucket.name;

                    return (
                      <Tooltip
                        key={bucket.name}
                        title={bucket.public_domain ? `https://${bucket.public_domain}` : null}
                        placement="right"
                      >
                        <div
                          className={`drawer-bucket-item ${isCurrentBucket ? 'current' : ''}`}
                          onClick={() => handleSelectBucket(token.id, bucket.name)}
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
        )}
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
} from '../stores/accountStore';
