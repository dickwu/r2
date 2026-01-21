'use client';

import { Modal, Alert, Button, Typography, Space, App } from 'antd';
import { DownloadOutlined, UploadOutlined } from '@ant-design/icons';
import { save, open as openDialog } from '@tauri-apps/plugin-dialog';
import { writeTextFile, readTextFile } from '@tauri-apps/plugin-fs';
import {
  useAccountStore,
  ProviderAccount,
} from '../stores/accountStore';

const { Text } = Typography;

const ACCOUNT_EXPORT_VERSION = 1;

interface AccountExportPayload {
  version: number;
  exported_at: string;
  accounts: ProviderAccount[];
}

interface AccountTransferModalProps {
  open: boolean;
  onClose: () => void;
}

export default function AccountTransferModal({ open, onClose }: AccountTransferModalProps) {
  const accounts = useAccountStore((state) => state.accounts);
  const createAccount = useAccountStore((state) => state.createAccount);
  const createToken = useAccountStore((state) => state.createToken);
  const saveBuckets = useAccountStore((state) => state.saveBuckets);
  const createAwsAccount = useAccountStore((state) => state.createAwsAccount);
  const saveAwsBuckets = useAccountStore((state) => state.saveAwsBuckets);
  const createMinioAccount = useAccountStore((state) => state.createMinioAccount);
  const saveMinioBuckets = useAccountStore((state) => state.saveMinioBuckets);
  const createRustfsAccount = useAccountStore((state) => state.createRustfsAccount);
  const saveRustfsBuckets = useAccountStore((state) => state.saveRustfsBuckets);

  const { message } = App.useApp();

  function buildExportPayload(): AccountExportPayload {
    return {
      version: ACCOUNT_EXPORT_VERSION,
      exported_at: new Date().toISOString(),
      accounts,
    };
  }

  function buildExportFilename() {
    const timestamp = new Date().toISOString().replace(/[:.]/g, '-');
    return `r2-accounts-${timestamp}.json`;
  }

  function extractAccountsFromPayload(payload: unknown): ProviderAccount[] | null {
    if (Array.isArray(payload)) {
      return payload as ProviderAccount[];
    }
    if (payload && typeof payload === 'object' && 'accounts' in payload) {
      const accountsValue = (payload as { accounts?: unknown }).accounts;
      if (Array.isArray(accountsValue)) {
        return accountsValue as ProviderAccount[];
      }
    }
    return null;
  }

  async function handleExportAccounts() {
    if (accounts.length === 0) {
      message.warning('No accounts to export');
      return;
    }
    try {
      const filePath = await save({
        defaultPath: buildExportFilename(),
        filters: [{ name: 'JSON', extensions: ['json'] }],
      });
      if (!filePath) return;

      const payload = buildExportPayload();
      const json = JSON.stringify(payload, null, 2);
      await writeTextFile(filePath, json);
      message.success('Accounts exported');
    } catch (e) {
      console.error('Failed to export accounts:', e);
      message.error('Failed to export accounts');
    }
  }

  async function handleImportAccounts(rawAccounts: ProviderAccount[]) {
    const existingR2Ids = new Set(
      accounts.filter((account) => account.provider === 'r2').map((account) => account.account.id)
    );
    const seenR2Ids = new Set<string>();
    let imported = 0;
    let skipped = 0;
    let failed = 0;
    let warnings = 0;

    for (const entry of rawAccounts) {
      if (!entry || typeof entry !== 'object' || !('provider' in entry)) {
        skipped += 1;
        continue;
      }

      const accountData = entry as ProviderAccount;
      try {
        if (accountData.provider === 'r2') {
          const account = accountData.account;
          if (!account?.id) {
            failed += 1;
            continue;
          }
          if (existingR2Ids.has(account.id) || seenR2Ids.has(account.id)) {
            skipped += 1;
            continue;
          }
          await createAccount(account.id, account.name || undefined);
          seenR2Ids.add(account.id);

          const tokenEntries = Array.isArray(accountData.tokens) ? accountData.tokens : [];
          for (const tokenEntry of tokenEntries) {
            const token = tokenEntry?.token;
            if (!token?.api_token || !token?.access_key_id || !token?.secret_access_key) {
              warnings += 1;
              continue;
            }
            try {
              const createdToken = await createToken({
                account_id: account.id,
                name: token.name || undefined,
                api_token: token.api_token,
                access_key_id: token.access_key_id,
                secret_access_key: token.secret_access_key,
              });
              const bucketEntries = Array.isArray(tokenEntry?.buckets) ? tokenEntry.buckets : [];
              if (bucketEntries.length > 0) {
                await saveBuckets(
                  createdToken.id,
                  bucketEntries
                    .filter((bucket) => bucket?.name)
                    .map((bucket) => ({
                      name: bucket.name,
                      public_domain: bucket.public_domain ?? null,
                      public_domain_scheme: bucket.public_domain_scheme ?? null,
                    }))
                );
              }
            } catch (e) {
              console.error('Failed to import R2 token:', e);
              warnings += 1;
            }
          }
          imported += 1;
        } else if (accountData.provider === 'aws') {
          const account = accountData.account;
          if (
            !account?.access_key_id ||
            !account?.secret_access_key ||
            !account?.region ||
            !account?.endpoint_scheme
          ) {
            failed += 1;
            continue;
          }
          const createdAccount = await createAwsAccount({
            name: account.name || undefined,
            access_key_id: account.access_key_id,
            secret_access_key: account.secret_access_key,
            region: account.region,
            endpoint_scheme: account.endpoint_scheme,
            endpoint_host: account.endpoint_host || null,
            force_path_style: account.force_path_style,
          });
          const bucketEntries = Array.isArray(accountData.buckets) ? accountData.buckets : [];
          if (bucketEntries.length > 0) {
            await saveAwsBuckets(
              createdAccount.id,
              bucketEntries
                .filter((bucket) => bucket?.name)
                .map((bucket) => ({
                  name: bucket.name,
                  public_domain_scheme: bucket.public_domain_scheme ?? null,
                  public_domain_host: bucket.public_domain_host ?? null,
                }))
            );
          }
          imported += 1;
        } else if (accountData.provider === 'minio') {
          const account = accountData.account;
          if (
            !account?.access_key_id ||
            !account?.secret_access_key ||
            !account?.endpoint_scheme ||
            !account?.endpoint_host
          ) {
            failed += 1;
            continue;
          }
          const createdAccount = await createMinioAccount({
            name: account.name || undefined,
            access_key_id: account.access_key_id,
            secret_access_key: account.secret_access_key,
            endpoint_scheme: account.endpoint_scheme,
            endpoint_host: account.endpoint_host,
            force_path_style: account.force_path_style,
          });
          const bucketEntries = Array.isArray(accountData.buckets) ? accountData.buckets : [];
          if (bucketEntries.length > 0) {
            await saveMinioBuckets(
              createdAccount.id,
              bucketEntries
                .filter((bucket) => bucket?.name)
                .map((bucket) => ({
                  name: bucket.name,
                  public_domain_scheme: bucket.public_domain_scheme ?? null,
                  public_domain_host: bucket.public_domain_host ?? null,
                }))
            );
          }
          imported += 1;
        } else if (accountData.provider === 'rustfs') {
          const account = accountData.account;
          if (
            !account?.access_key_id ||
            !account?.secret_access_key ||
            !account?.endpoint_scheme ||
            !account?.endpoint_host
          ) {
            failed += 1;
            continue;
          }
          const createdAccount = await createRustfsAccount({
            name: account.name || undefined,
            access_key_id: account.access_key_id,
            secret_access_key: account.secret_access_key,
            endpoint_scheme: account.endpoint_scheme,
            endpoint_host: account.endpoint_host,
          });
          const bucketEntries = Array.isArray(accountData.buckets) ? accountData.buckets : [];
          if (bucketEntries.length > 0) {
            await saveRustfsBuckets(
              createdAccount.id,
              bucketEntries
                .filter((bucket) => bucket?.name)
                .map((bucket) => ({
                  name: bucket.name,
                  public_domain_scheme: bucket.public_domain_scheme ?? null,
                  public_domain_host: bucket.public_domain_host ?? null,
                }))
            );
          }
          imported += 1;
        } else {
          skipped += 1;
        }
      } catch (e) {
        console.error('Failed to import account:', e);
        failed += 1;
      }
    }

    if (imported === 0 && skipped === 0 && failed === 0) {
      message.warning('No valid accounts found to import');
      return;
    }

    if (failed > 0 || warnings > 0) {
      message.warning(
        `Imported ${imported} account${imported !== 1 ? 's' : ''}, ` +
          `${skipped} skipped, ${failed} failed, ${warnings} warning${warnings !== 1 ? 's' : ''}`
      );
    } else {
      message.success(
        `Imported ${imported} account${imported !== 1 ? 's' : ''}` +
          (skipped > 0 ? `, ${skipped} skipped` : '')
      );
    }
  }

  async function handleTriggerImport() {
    try {
      const filePath = await openDialog({
        multiple: false,
        filters: [{ name: 'JSON', extensions: ['json'] }],
      });
      if (!filePath) return;

      const text = await readTextFile(filePath as string);
      const parsed = JSON.parse(text);
      const rawAccounts = extractAccountsFromPayload(parsed);
      if (!rawAccounts) {
        message.error('Invalid account export file');
        return;
      }
      await handleImportAccounts(rawAccounts);
    } catch (e) {
      console.error('Failed to import accounts:', e);
      message.error('Failed to import accounts');
    }
  }

  return (
    <Modal
      open={open}
      onCancel={onClose}
      footer={null}
      title="Import / Export Accounts"
      centered
    >
      <Space orientation="vertical" size="middle" style={{ width: '100%' }}>
        <Alert
          type="warning"
          showIcon
          title="Export includes access keys, secrets, and tokens. Store the JSON securely."
        />
        <Button block icon={<DownloadOutlined />} onClick={handleExportAccounts}>
          Export JSON
        </Button>
        <Button block icon={<UploadOutlined />} onClick={handleTriggerImport}>
          Import JSON
        </Button>
        <Text type="secondary" style={{ fontSize: 12 }}>
          Import adds accounts without deleting existing ones. R2 accounts with the same ID are
          skipped.
        </Text>
      </Space>
    </Modal>
  );
}
