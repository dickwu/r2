'use client';

import { useTranslation } from 'react-i18next';
import { useState, useMemo } from 'react';
import { App } from 'antd';
import type { MenuProps } from 'antd';
import {
  PlusOutlined,
  EditOutlined,
  DeleteOutlined,
  SettingOutlined,
  MenuFoldOutlined,
  SearchOutlined,
  SwapOutlined,
  LinkOutlined,
  DisconnectOutlined,
  FolderOpenOutlined,
} from '@ant-design/icons';
import { useAccountStore, Token, ProviderAccount } from '@/app/stores/accountStore';
import { useMountStore, findMount, type MountTarget } from '@/app/stores/mountStore';
import { detectOs, revealActionLabel } from '@/app/utils/mount';
import { useThemeStore } from '@/app/stores/themeStore';
import { useCurrentPathStore } from '@/app/stores/currentPathStore';
import AccountTransferModal from '@/app/components/AccountTransferModal';
import {
  AccountRow,
  R2AccountChildren,
  NonR2AccountChildren,
} from '@/app/components/AccountSidebarRows';
import type { SettingsTab } from '@/app/components/SettingsModal';

interface AccountSidebarProps {
  onAddAccount: () => void;
  onEditAccount: (account: ProviderAccount) => void;
  onAddToken: (accountId: string) => void;
  onEditToken: (token: Token) => void;
  onOpenSettings?: (tab?: SettingsTab) => void;
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

  const mounts = useMountStore((state) => state.mounts);
  const openMountModal = useMountStore((state) => state.openMountModal);
  const unmountBucket = useMountStore((state) => state.unmount);

  const sidebarStyle = useThemeStore((state) => state.sidebarStyle);
  const cycleSidebarStyle = useThemeStore((state) => state.cycleSidebarStyle);
  const setSidebarStyle = useThemeStore((state) => state.setSidebarStyle);
  const setCurrentPath = useCurrentPathStore((state) => state.setCurrentPath);

  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [search, setSearch] = useState('');
  const [transferModalOpen, setTransferModalOpen] = useState(false);

  const { message, modal } = App.useApp();
  const { t } = useTranslation();

  const os = useMemo(() => detectOs(), []);

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
      message.error(t('sidebar.switchFailed'));
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
      message.error(t('sidebar.switchFailed'));
    }
  }

  async function handleDeleteAccount(accountData: ProviderAccount) {
    modal.confirm({
      title: t('sidebar.deleteAccount'),
      content: t('sidebar.deleteAccountConfirm'),
      okText: t('files.action.delete'),
      cancelText: t('common.cancel'),
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
          message.success(t('sidebar.accountDeleted'));
        } catch {
          message.error(t('sidebar.accountDeleteFailed'));
        }
      },
    });
  }

  async function handleDeleteToken(tokenId: number) {
    modal.confirm({
      title: t('sidebar.deleteToken'),
      content: t('sidebar.deleteTokenConfirm'),
      okText: t('files.action.delete'),
      cancelText: t('common.cancel'),
      okButtonProps: { danger: true },
      onOk: async () => {
        try {
          await deleteToken(tokenId);
          message.success(t('sidebar.tokenDeleted'));
        } catch {
          message.error(t('sidebar.tokenDeleteFailed'));
        }
      },
    });
  }

  function getAccountContextMenu(accountData: ProviderAccount): MenuProps['items'] {
    const items: MenuProps['items'] = [
      {
        key: 'edit',
        label: t('sidebar.editAccount'),
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
        label: t('sidebar.addToken'),
        icon: <PlusOutlined />,
        onClick: (e) => {
          e.domEvent.stopPropagation();
          onAddToken(accountData.account.id);
        },
      });
    }

    items.push({
      key: 'transfer',
      label: t('sidebar.transferAccounts'),
      icon: <SwapOutlined />,
      onClick: (e) => {
        e.domEvent.stopPropagation();
        setTransferModalOpen(true);
      },
    });

    items.push({ type: 'divider' });
    items.push({
      key: 'delete',
      label: t('sidebar.deleteAccount'),
      icon: <DeleteOutlined />,
      danger: true,
      onClick: (e) => {
        e.domEvent.stopPropagation();
        handleDeleteAccount(accountData);
      },
    });

    return items;
  }

  /**
   * Assemble mount credentials for any listed bucket: R2 keys live on the
   * token, every other provider's on the account. Endpoint/region assembly
   * mirrors toStorageConfig().
   *
   * Must stay free of select*Bucket / set_current_* — mounting a bucket may
   * never move the user's current selection as a side effect. The account and
   * token rows already hold every credential this needs.
   */
  function buildMountTarget(
    accountData: ProviderAccount,
    bucketName: string,
    token?: Token
  ): MountTarget {
    const accountLabel = accountData.account.name || accountData.account.id;

    if (accountData.provider === 'r2') {
      return {
        provider: 'r2',
        accountId: accountData.account.id,
        accountLabel,
        bucket: bucketName,
        accessKeyId: token?.access_key_id ?? '',
        secretAccessKey: token?.secret_access_key ?? '',
        region: null,
        endpointUrl: null,
        forcePathStyle: null,
      };
    }

    if (accountData.provider === 'aws') {
      const account = accountData.account;
      return {
        provider: 'aws',
        accountId: account.id,
        accountLabel,
        bucket: bucketName,
        accessKeyId: account.access_key_id,
        secretAccessKey: account.secret_access_key,
        region: account.region,
        endpointUrl: account.endpoint_host
          ? `${account.endpoint_scheme || 'https'}://${account.endpoint_host}`
          : null,
        forcePathStyle: account.force_path_style,
      };
    }

    const account = accountData.account;
    return {
      provider: accountData.provider,
      accountId: account.id,
      accountLabel,
      bucket: bucketName,
      accessKeyId: account.access_key_id,
      secretAccessKey: account.secret_access_key,
      region: null,
      endpointUrl: `${account.endpoint_scheme}://${account.endpoint_host}`,
      forcePathStyle: accountData.provider === 'rustfs' ? true : account.force_path_style,
    };
  }

  async function handleRevealMount(localPath: string) {
    try {
      const { revealItemInDir } = await import('@tauri-apps/plugin-opener');
      await revealItemInDir(localPath);
    } catch {
      try {
        const { openPath } = await import('@tauri-apps/plugin-opener');
        await openPath(localPath);
      } catch (e) {
        console.error('Failed to open mount folder:', e);
        message.error(t('sidebar.openFolderFailed'));
      }
    }
  }

  async function handleUnmountBucket(mountId: string, bucketName: string) {
    const ok = await unmountBucket(mountId);
    if (ok) {
      message.success(t('sidebar.unmounted', { bucket: bucketName }));
    } else {
      message.error(
        useMountStore.getState().error || t('sidebar.unmountFailed', { bucket: bucketName })
      );
    }
  }

  function getBucketContextMenu(
    accountData: ProviderAccount,
    bucketName: string,
    token?: Token
  ): MenuProps['items'] {
    const mount = findMount(mounts, accountData.provider, accountData.account.id, bucketName);

    if (mount) {
      return [
        {
          key: 'reveal',
          label: revealActionLabel(os),
          icon: <FolderOpenOutlined />,
          onClick: (e) => {
            e.domEvent.stopPropagation();
            handleRevealMount(mount.localPath);
          },
        },
        { type: 'divider' },
        {
          key: 'unmount',
          label: t('sidebar.unmount'),
          icon: <DisconnectOutlined />,
          onClick: (e) => {
            e.domEvent.stopPropagation();
            handleUnmountBucket(mount.mountId, bucketName);
          },
        },
      ];
    }

    return [
      {
        key: 'mount',
        label: t('sidebar.mountAsDrive'),
        icon: <LinkOutlined />,
        onClick: (e) => {
          e.domEvent.stopPropagation();
          openMountModal(buildMountTarget(accountData, bucketName, token));
        },
      },
    ];
  }

  function getMountedPath(accountData: ProviderAccount, bucketName: string): string | null {
    return (
      findMount(mounts, accountData.provider, accountData.account.id, bucketName)?.localPath ?? null
    );
  }

  function getTokenContextMenu(token: Token): MenuProps['items'] {
    return [
      {
        key: 'edit',
        label: t('sidebar.editToken'),
        icon: <EditOutlined />,
        onClick: () => onEditToken(token),
      },
      { type: 'divider' },
      {
        key: 'delete',
        label: t('sidebar.deleteToken'),
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
              title={t('sidebar.cycleStyle')}
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
                placeholder={t('sidebar.filter')}
                value={search}
                onChange={(e) => setSearch(e.target.value)}
              />
            </div>
            <button
              className="sb-icon-btn"
              title={t('sidebar.cycleStyle')}
              onClick={cycleSidebarStyle}
            >
              <MenuFoldOutlined style={{ fontSize: 14 }} />
            </button>
          </div>
        )}

        {/* Section label */}
        {!collapsed && <div className="sb-section-label">{t('sidebar.accounts')}</div>}

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
                    getBucketContextMenu={(token, bucketName) =>
                      getBucketContextMenu(accountData, bucketName, token)
                    }
                    getMountedPath={(bucketName) => getMountedPath(accountData, bucketName)}
                    onOpenMounts={onOpenSettings && (() => onOpenSettings('mounts'))}
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
                    getBucketContextMenu={(bucketName) =>
                      getBucketContextMenu(accountData, bucketName)
                    }
                    getMountedPath={(bucketName) => getMountedPath(accountData, bucketName)}
                    onOpenMounts={onOpenSettings && (() => onOpenSettings('mounts'))}
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
              {searchLower ? t('sidebar.noMatching') : t('accounts.noAccounts')}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="sb-footer">
          <button
            className="sb-footer-btn"
            onClick={() => onOpenSettings?.('account') ?? onAddAccount()}
            title={collapsed ? t('accounts.addAccount') : undefined}
          >
            <PlusOutlined style={{ fontSize: collapsed ? 15 : 11 }} />
            {!collapsed && <span>{t('accounts.addAccount')}</span>}
          </button>
          <button
            className="sb-icon-btn"
            onClick={() => onOpenSettings?.('account')}
            title={t('settings.title')}
            style={collapsed ? undefined : { width: 30, height: 30 }}
          >
            <SettingOutlined style={{ fontSize: collapsed ? 16 : 14 }} />
          </button>
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
