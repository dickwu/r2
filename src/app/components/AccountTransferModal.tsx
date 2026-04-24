'use client';

import { useMemo, useState } from 'react';
import {
  Modal,
  Alert,
  Button,
  Typography,
  Space,
  App,
  Tabs,
  Collapse,
  Checkbox,
  Table,
  Tag,
  Select,
  Tooltip,
} from 'antd';
import { DownloadOutlined, UploadOutlined, FileAddOutlined } from '@ant-design/icons';
import { save, open as openDialog } from '@tauri-apps/plugin-dialog';
import { writeTextFile, readTextFile } from '@tauri-apps/plugin-fs';
import { useAccountStore, ProviderAccount } from '@/app/stores/accountStore';

const { Text } = Typography;

const ACCOUNT_EXPORT_VERSION = 2;

type ProviderKey = 'r2' | 'aws' | 'minio' | 'rustfs';

const PROVIDER_ORDER: ProviderKey[] = ['r2', 'aws', 'minio', 'rustfs'];

const PROVIDER_LABELS: Record<ProviderKey, string> = {
  r2: 'Cloudflare R2',
  aws: 'AWS S3',
  minio: 'MinIO',
  rustfs: 'RustFS',
};

interface SelectionSummary {
  r2: number;
  aws: number;
  minio: number;
  rustfs: number;
}

interface AccountExportPayload {
  version: number;
  exported_at: string;
  selection_summary?: SelectionSummary;
  accounts: ProviderAccount[];
}

interface AccountTransferModalProps {
  open: boolean;
  onClose: () => void;
}

type RowAction = 'skip' | 'import' | 'overwrite' | 'duplicate';
type Classification = 'new' | 'conflict' | 'invalid';

interface PreviewRow {
  rowId: string;
  classification: Classification;
  invalidReason?: string;
  account?: ProviderAccount;
  identity?: string;
  provider: ProviderKey | 'unknown';
  name: string;
  identifierPreview: string;
  action: RowAction;
}

interface ApplyResult {
  imported: number;
  overwritten: number;
  duplicated: number;
  skipped: number;
  failed: number;
}

type ValidationResult =
  | { ok: true; account: ProviderAccount }
  | { ok: false; reason: string; providerHint?: ProviderKey };

function accountIdentity(account: ProviderAccount): string {
  switch (account.provider) {
    case 'r2':
      return `r2:${account.account.id}`;
    case 'aws':
      return `aws:${account.account.access_key_id}`;
    case 'minio':
      return `minio:${account.account.access_key_id}`;
    case 'rustfs':
      return `rustfs:${account.account.access_key_id}`;
  }
}

function validateAccount(entry: unknown): ValidationResult {
  if (!entry || typeof entry !== 'object' || !('provider' in entry)) {
    return { ok: false, reason: 'Missing provider field' };
  }
  const data = entry as ProviderAccount;
  if (data.provider === 'r2') {
    if (!data.account?.id) {
      return { ok: false, reason: 'Missing R2 account id', providerHint: 'r2' };
    }
    return { ok: true, account: data };
  }
  if (data.provider === 'aws') {
    const a = data.account;
    if (!a?.access_key_id || !a?.secret_access_key || !a?.region || !a?.endpoint_scheme) {
      return { ok: false, reason: 'Missing required AWS field', providerHint: 'aws' };
    }
    return { ok: true, account: data };
  }
  if (data.provider === 'minio') {
    const a = data.account;
    if (!a?.access_key_id || !a?.secret_access_key || !a?.endpoint_scheme || !a?.endpoint_host) {
      return { ok: false, reason: 'Missing required MinIO field', providerHint: 'minio' };
    }
    return { ok: true, account: data };
  }
  if (data.provider === 'rustfs') {
    const a = data.account;
    if (!a?.access_key_id || !a?.secret_access_key || !a?.endpoint_scheme || !a?.endpoint_host) {
      return { ok: false, reason: 'Missing required RustFS field', providerHint: 'rustfs' };
    }
    return { ok: true, account: data };
  }
  return { ok: false, reason: 'Unknown provider' };
}

function maskKey(key: string): string {
  if (!key) return '';
  if (key.length <= 4) return `****${key}`;
  return `****${key.slice(-4)}`;
}

function accountDisplayName(account: ProviderAccount): string {
  return account.account.name || 'Untitled';
}

function accountIdentifierPreview(account: ProviderAccount): string {
  switch (account.provider) {
    case 'r2':
      return account.account.id;
    case 'aws': {
      const region = account.account.region || account.account.endpoint_host || 'aws';
      return `${region} · ${maskKey(account.account.access_key_id)}`;
    }
    case 'minio':
    case 'rustfs':
      return `${account.account.endpoint_host} · ${maskKey(account.account.access_key_id)}`;
  }
}

function nameWithSuffix(base: string, taken: Set<string>): string {
  const first = `${base} (imported)`;
  if (!taken.has(first)) return first;
  let n = 2;
  while (taken.has(`${base} (imported ${n})`)) n += 1;
  return `${base} (imported ${n})`;
}

function extractAccountsFromPayload(payload: unknown): unknown[] | null {
  if (Array.isArray(payload)) return payload;
  if (payload && typeof payload === 'object' && 'accounts' in payload) {
    const value = (payload as { accounts?: unknown }).accounts;
    if (Array.isArray(value)) return value;
  }
  return null;
}

function buildExportFilename() {
  const timestamp = new Date().toISOString().replace(/[:.]/g, '-');
  return `r2-accounts-${timestamp}.json`;
}

async function createAccountWithBuckets(
  account: ProviderAccount,
  nameOverride: string | undefined
) {
  const store = useAccountStore.getState();
  if (account.provider === 'r2') {
    const id = account.account.id;
    await store.createAccount(id, nameOverride);
    for (const tokenEntry of account.tokens || []) {
      const tk = tokenEntry?.token;
      if (!tk?.api_token || !tk?.access_key_id || !tk?.secret_access_key) continue;
      const created = await store.createToken({
        account_id: id,
        name: tk.name || undefined,
        api_token: tk.api_token,
        access_key_id: tk.access_key_id,
        secret_access_key: tk.secret_access_key,
      });
      const buckets = (tokenEntry.buckets || []).filter((b) => b?.name);
      if (buckets.length > 0) {
        await store.saveBuckets(
          created.id,
          buckets.map((b) => ({
            name: b.name,
            public_domain: b.public_domain ?? null,
            public_domain_scheme: b.public_domain_scheme ?? null,
          }))
        );
      }
    }
    return;
  }
  if (account.provider === 'aws') {
    const a = account.account;
    const created = await store.createAwsAccount({
      name: nameOverride,
      access_key_id: a.access_key_id,
      secret_access_key: a.secret_access_key,
      region: a.region,
      endpoint_scheme: a.endpoint_scheme,
      endpoint_host: a.endpoint_host,
      force_path_style: a.force_path_style,
    });
    await saveS3Buckets('aws', created.id, account.buckets || []);
    return;
  }
  if (account.provider === 'minio') {
    const a = account.account;
    const created = await store.createMinioAccount({
      name: nameOverride,
      access_key_id: a.access_key_id,
      secret_access_key: a.secret_access_key,
      endpoint_scheme: a.endpoint_scheme,
      endpoint_host: a.endpoint_host,
      force_path_style: a.force_path_style,
    });
    await saveS3Buckets('minio', created.id, account.buckets || []);
    return;
  }
  if (account.provider === 'rustfs') {
    const a = account.account;
    const created = await store.createRustfsAccount({
      name: nameOverride,
      access_key_id: a.access_key_id,
      secret_access_key: a.secret_access_key,
      endpoint_scheme: a.endpoint_scheme,
      endpoint_host: a.endpoint_host,
    });
    await saveS3Buckets('rustfs', created.id, account.buckets || []);
  }
}

async function saveS3Buckets(
  provider: 'aws' | 'minio' | 'rustfs',
  accountId: string,
  buckets: {
    name: string;
    public_domain_scheme?: string | null;
    public_domain_host?: string | null;
  }[]
) {
  const store = useAccountStore.getState();
  const filtered = (buckets || [])
    .filter((b) => b?.name)
    .map((b) => ({
      name: b.name,
      public_domain_scheme: b.public_domain_scheme ?? null,
      public_domain_host: b.public_domain_host ?? null,
    }));
  if (filtered.length === 0) return;
  if (provider === 'aws') await store.saveAwsBuckets(accountId, filtered);
  else if (provider === 'minio') await store.saveMinioBuckets(accountId, filtered);
  else await store.saveRustfsBuckets(accountId, filtered);
}

async function overwriteAccountWithBuckets(account: ProviderAccount) {
  const store = useAccountStore.getState();
  if (account.provider === 'r2') {
    const id = account.account.id;
    await store.updateAccount(id, account.account.name || undefined);
    const fresh = useAccountStore.getState().accounts;
    const local = fresh.find((a) => a.provider === 'r2' && a.account.id === id);
    const existingTokensByKey = new Map<string, number>();
    if (local && local.provider === 'r2') {
      for (const t of local.tokens) {
        existingTokensByKey.set(t.token.access_key_id, t.token.id);
      }
    }
    for (const tokenEntry of account.tokens || []) {
      const tk = tokenEntry?.token;
      if (!tk?.api_token || !tk?.access_key_id || !tk?.secret_access_key) continue;
      const matchedId = existingTokensByKey.get(tk.access_key_id);
      let tokenId: number;
      if (matchedId !== undefined) {
        await store.updateToken({
          id: matchedId,
          name: tk.name || undefined,
          api_token: tk.api_token,
          access_key_id: tk.access_key_id,
          secret_access_key: tk.secret_access_key,
        });
        tokenId = matchedId;
      } else {
        const created = await store.createToken({
          account_id: id,
          name: tk.name || undefined,
          api_token: tk.api_token,
          access_key_id: tk.access_key_id,
          secret_access_key: tk.secret_access_key,
        });
        tokenId = created.id;
      }
      const buckets = (tokenEntry.buckets || []).filter((b) => b?.name);
      if (buckets.length > 0) {
        await store.saveBuckets(
          tokenId,
          buckets.map((b) => ({
            name: b.name,
            public_domain: b.public_domain ?? null,
            public_domain_scheme: b.public_domain_scheme ?? null,
          }))
        );
      }
    }
    return;
  }
  if (account.provider === 'aws') {
    const a = account.account;
    const local = useAccountStore
      .getState()
      .accounts.find((x) => x.provider === 'aws' && x.account.access_key_id === a.access_key_id);
    if (!local || local.provider !== 'aws') {
      await createAccountWithBuckets(account, a.name || undefined);
      return;
    }
    await store.updateAwsAccount({
      id: local.account.id,
      name: a.name || undefined,
      access_key_id: a.access_key_id,
      secret_access_key: a.secret_access_key,
      region: a.region,
      endpoint_scheme: a.endpoint_scheme,
      endpoint_host: a.endpoint_host,
      force_path_style: a.force_path_style,
    });
    await saveS3Buckets('aws', local.account.id, account.buckets || []);
    return;
  }
  if (account.provider === 'minio') {
    const a = account.account;
    const local = useAccountStore
      .getState()
      .accounts.find((x) => x.provider === 'minio' && x.account.access_key_id === a.access_key_id);
    if (!local || local.provider !== 'minio') {
      await createAccountWithBuckets(account, a.name || undefined);
      return;
    }
    await store.updateMinioAccount({
      id: local.account.id,
      name: a.name || undefined,
      access_key_id: a.access_key_id,
      secret_access_key: a.secret_access_key,
      endpoint_scheme: a.endpoint_scheme,
      endpoint_host: a.endpoint_host,
      force_path_style: a.force_path_style,
    });
    await saveS3Buckets('minio', local.account.id, account.buckets || []);
    return;
  }
  if (account.provider === 'rustfs') {
    const a = account.account;
    const local = useAccountStore
      .getState()
      .accounts.find((x) => x.provider === 'rustfs' && x.account.access_key_id === a.access_key_id);
    if (!local || local.provider !== 'rustfs') {
      await createAccountWithBuckets(account, a.name || undefined);
      return;
    }
    await store.updateRustfsAccount({
      id: local.account.id,
      name: a.name || undefined,
      access_key_id: a.access_key_id,
      secret_access_key: a.secret_access_key,
      endpoint_scheme: a.endpoint_scheme,
      endpoint_host: a.endpoint_host,
    });
    await saveS3Buckets('rustfs', local.account.id, account.buckets || []);
  }
}

async function applyImport(rows: PreviewRow[], existingNames: Set<string>): Promise<ApplyResult> {
  const taken = new Set(existingNames);
  const result: ApplyResult = {
    imported: 0,
    overwritten: 0,
    duplicated: 0,
    skipped: 0,
    failed: 0,
  };
  for (const row of rows) {
    if (row.classification === 'invalid' || row.action === 'skip' || !row.account) {
      if (row.action === 'skip' || row.classification === 'invalid') result.skipped += 1;
      continue;
    }
    try {
      if (row.action === 'duplicate') {
        const baseName = row.account.account.name || PROVIDER_LABELS[row.account.provider];
        const newName = nameWithSuffix(baseName, taken);
        taken.add(newName);
        await createAccountWithBuckets(row.account, newName);
        result.duplicated += 1;
      } else if (row.action === 'overwrite') {
        await overwriteAccountWithBuckets(row.account);
        if (row.account.account.name) taken.add(row.account.account.name);
        result.overwritten += 1;
      } else if (row.action === 'import') {
        const name = row.account.account.name ?? undefined;
        if (name) taken.add(name);
        await createAccountWithBuckets(row.account, name);
        result.imported += 1;
      }
    } catch (e) {
      console.error('Failed to import account row:', e);
      result.failed += 1;
    }
  }
  return result;
}

function ExportPanel() {
  const accounts = useAccountStore((s) => s.accounts);
  const { message } = App.useApp();
  const [selected, setSelected] = useState<Set<string>>(() => new Set());
  const [exporting, setExporting] = useState(false);

  const grouped = useMemo(() => {
    const groups: Record<ProviderKey, ProviderAccount[]> = {
      r2: [],
      aws: [],
      minio: [],
      rustfs: [],
    };
    for (const account of accounts) groups[account.provider].push(account);
    return groups;
  }, [accounts]);

  function toggle(identity: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(identity)) next.delete(identity);
      else next.add(identity);
      return next;
    });
  }

  function setProviderSelection(provider: ProviderKey, value: boolean) {
    setSelected((prev) => {
      const next = new Set(prev);
      for (const acc of grouped[provider]) {
        const id = accountIdentity(acc);
        if (value) next.add(id);
        else next.delete(id);
      }
      return next;
    });
  }

  async function handleExport() {
    const picked = accounts.filter((a) => selected.has(accountIdentity(a)));
    if (picked.length === 0) {
      message.warning('Select at least one account to export');
      return;
    }
    setExporting(true);
    try {
      const filePath = await save({
        defaultPath: buildExportFilename(),
        filters: [{ name: 'JSON', extensions: ['json'] }],
      });
      if (!filePath) return;
      const summary: SelectionSummary = { r2: 0, aws: 0, minio: 0, rustfs: 0 };
      for (const a of picked) summary[a.provider] += 1;
      const payload: AccountExportPayload = {
        version: ACCOUNT_EXPORT_VERSION,
        exported_at: new Date().toISOString(),
        selection_summary: summary,
        accounts: picked,
      };
      await writeTextFile(filePath, JSON.stringify(payload, null, 2));
      message.success(`Exported ${picked.length} account${picked.length !== 1 ? 's' : ''}`);
    } catch (e) {
      console.error('Failed to export accounts:', e);
      message.error('Failed to export accounts');
    } finally {
      setExporting(false);
    }
  }

  if (accounts.length === 0) {
    return <Alert type="info" showIcon title="No accounts to export. Add an account first." />;
  }

  const collapseItems = PROVIDER_ORDER.map((provider) => {
    const list = grouped[provider];
    const allIds = list.map(accountIdentity);
    const selectedCount = allIds.filter((id) => selected.has(id)).length;
    const allSelected = list.length > 0 && selectedCount === list.length;
    const someSelected = selectedCount > 0 && !allSelected;
    return {
      key: provider,
      label: (
        <Space>
          <Checkbox
            disabled={list.length === 0}
            checked={allSelected}
            indeterminate={someSelected}
            onClick={(e) => e.stopPropagation()}
            onChange={(e) => setProviderSelection(provider, e.target.checked)}
          />
          <span>{PROVIDER_LABELS[provider]}</span>
          <Tag>
            {selectedCount}/{list.length}
          </Tag>
        </Space>
      ),
      children:
        list.length === 0 ? (
          <Text type="secondary">No accounts</Text>
        ) : (
          <Space orientation="vertical" style={{ width: '100%' }}>
            {list.map((acc) => {
              const id = accountIdentity(acc);
              return (
                <Checkbox key={id} checked={selected.has(id)} onChange={() => toggle(id)}>
                  <Space size="small">
                    <span>{accountDisplayName(acc)}</span>
                    <Text type="secondary" style={{ fontSize: 12 }}>
                      {accountIdentifierPreview(acc)}
                    </Text>
                  </Space>
                </Checkbox>
              );
            })}
          </Space>
        ),
    };
  });

  const defaultActiveKeys = PROVIDER_ORDER.filter((p) => grouped[p].length > 0);

  return (
    <Space orientation="vertical" size="middle" style={{ width: '100%' }}>
      <Collapse defaultActiveKey={defaultActiveKeys} items={collapseItems} />
      <Button
        type="primary"
        block
        icon={<DownloadOutlined />}
        loading={exporting}
        disabled={selected.size === 0}
        onClick={handleExport}
      >
        Export {selected.size} selected
      </Button>
    </Space>
  );
}

function buildPreviewRow(entry: unknown, idx: number, existingIdentities: Set<string>): PreviewRow {
  const validation = validateAccount(entry);
  const rowId = `row-${idx}`;
  if (!validation.ok) {
    const provider = validation.providerHint ?? 'unknown';
    const fallbackName =
      entry && typeof entry === 'object' && 'account' in entry
        ? ((entry as { account?: { name?: string } }).account?.name ?? 'Untitled')
        : 'Untitled';
    return {
      rowId,
      classification: 'invalid',
      invalidReason: validation.reason,
      provider,
      name: fallbackName,
      identifierPreview: validation.reason,
      action: 'skip',
    };
  }
  const account = validation.account;
  const identity = accountIdentity(account);
  const isConflict = existingIdentities.has(identity);
  return {
    rowId,
    classification: isConflict ? 'conflict' : 'new',
    account,
    identity,
    provider: account.provider,
    name: accountDisplayName(account),
    identifierPreview: accountIdentifierPreview(account),
    action: isConflict ? 'skip' : 'import',
  };
}

function ImportPanel({ onApplied }: { onApplied?: () => void }) {
  const accounts = useAccountStore((s) => s.accounts);
  const { message, modal } = App.useApp();
  const [rows, setRows] = useState<PreviewRow[] | null>(null);
  const [applying, setApplying] = useState(false);

  const existingIdentities = useMemo(() => {
    const set = new Set<string>();
    for (const a of accounts) set.add(accountIdentity(a));
    return set;
  }, [accounts]);

  const existingNames = useMemo(() => {
    const set = new Set<string>();
    for (const a of accounts) {
      if (a.account.name) set.add(a.account.name);
    }
    return set;
  }, [accounts]);

  async function handlePickFile() {
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
      const built = rawAccounts.map((entry, idx) =>
        buildPreviewRow(entry, idx, existingIdentities)
      );
      if (built.length === 0) {
        message.warning('No accounts found in file');
        return;
      }
      setRows(built);
    } catch (e) {
      console.error('Failed to read import file:', e);
      message.error('Failed to read file');
    }
  }

  function setRowAction(rowId: string, action: RowAction) {
    setRows((prev) => prev?.map((r) => (r.rowId === rowId ? { ...r, action } : r)) ?? null);
  }

  function bulkSet(predicate: (row: PreviewRow) => boolean, action: RowAction) {
    setRows(
      (prev) =>
        prev?.map((r) => (r.classification !== 'invalid' && predicate(r) ? { ...r, action } : r)) ??
        null
    );
  }

  async function handleApply() {
    if (!rows) return;
    let toImport = 0;
    let toOverwrite = 0;
    let toDuplicate = 0;
    for (const r of rows) {
      if (r.classification === 'invalid') continue;
      if (r.action === 'import') toImport += 1;
      else if (r.action === 'overwrite') toOverwrite += 1;
      else if (r.action === 'duplicate') toDuplicate += 1;
    }
    if (toImport + toOverwrite + toDuplicate === 0) {
      message.warning('Nothing selected to import');
      return;
    }
    if (toOverwrite > 0) {
      const confirmed = await new Promise<boolean>((resolve) => {
        modal.confirm({
          title: 'Confirm overwrite',
          content: `${toOverwrite} existing account${
            toOverwrite !== 1 ? 's' : ''
          } will be overwritten with the imported version. This cannot be undone.`,
          okText: 'Overwrite',
          okButtonProps: { danger: true },
          cancelText: 'Cancel',
          onOk: () => resolve(true),
          onCancel: () => resolve(false),
        });
      });
      if (!confirmed) return;
    }
    setApplying(true);
    try {
      const result = await applyImport(rows, existingNames);
      const parts: string[] = [];
      if (result.imported) parts.push(`${result.imported} imported`);
      if (result.overwritten) parts.push(`${result.overwritten} overwritten`);
      if (result.duplicated) parts.push(`${result.duplicated} duplicated`);
      if (result.skipped) parts.push(`${result.skipped} skipped`);
      if (result.failed) parts.push(`${result.failed} failed`);
      const summary = parts.join(', ') || 'No changes';
      if (result.failed > 0) message.warning(summary);
      else message.success(summary);
      setRows(null);
      onApplied?.();
    } catch (e) {
      console.error('Failed to apply import:', e);
      message.error('Import failed');
    } finally {
      setApplying(false);
    }
  }

  if (!rows) {
    return (
      <div style={{ textAlign: 'center', padding: '32px 12px' }}>
        <Button type="primary" icon={<FileAddOutlined />} onClick={handlePickFile}>
          Choose JSON file…
        </Button>
        <div style={{ marginTop: 8 }}>
          <Text type="secondary" style={{ fontSize: 12 }}>
            Pick an export file to preview the accounts inside.
          </Text>
        </div>
      </div>
    );
  }

  let toImport = 0;
  let toOverwrite = 0;
  let toDuplicate = 0;
  let invalidCount = 0;
  for (const r of rows) {
    if (r.classification === 'invalid') {
      invalidCount += 1;
      continue;
    }
    if (r.action === 'import') toImport += 1;
    else if (r.action === 'overwrite') toOverwrite += 1;
    else if (r.action === 'duplicate') toDuplicate += 1;
  }
  const totalActionable = toImport + toOverwrite + toDuplicate;

  const columns = [
    {
      title: 'Provider',
      dataIndex: 'provider',
      width: 120,
      render: (p: ProviderRow['provider']) => (
        <Tag>{p === 'unknown' ? 'Unknown' : PROVIDER_LABELS[p]}</Tag>
      ),
    },
    {
      title: 'Name',
      dataIndex: 'name',
      ellipsis: true,
    },
    {
      title: 'Identifier',
      dataIndex: 'identifierPreview',
      ellipsis: true,
      render: (v: string) => (
        <Text type="secondary" style={{ fontSize: 12 }}>
          {v}
        </Text>
      ),
    },
    {
      title: 'Status',
      dataIndex: 'classification',
      width: 110,
      render: (c: Classification, row: PreviewRow) => {
        if (c === 'new') return <Tag color="green">New</Tag>;
        if (c === 'conflict') return <Tag color="orange">Conflict</Tag>;
        return (
          <Tooltip title={row.invalidReason}>
            <Tag color="red">Invalid</Tag>
          </Tooltip>
        );
      },
    },
    {
      title: 'Action',
      key: 'action',
      width: 200,
      render: (_: unknown, row: PreviewRow) => {
        if (row.classification === 'invalid') {
          return (
            <Text type="secondary" style={{ fontSize: 12 }}>
              {row.invalidReason}
            </Text>
          );
        }
        const options =
          row.classification === 'new'
            ? [
                { value: 'skip', label: 'Skip' },
                { value: 'import', label: 'Import' },
                { value: 'duplicate', label: 'Import as duplicate' },
              ]
            : [
                { value: 'skip', label: 'Skip' },
                { value: 'overwrite', label: 'Overwrite local' },
                { value: 'duplicate', label: 'Import as duplicate' },
              ];
        return (
          <Select
            size="small"
            style={{ width: '100%' }}
            value={row.action}
            options={options}
            onChange={(v) => setRowAction(row.rowId, v as RowAction)}
          />
        );
      },
    },
  ];

  return (
    <Space orientation="vertical" size="middle" style={{ width: '100%' }}>
      <Space wrap>
        <Button size="small" onClick={() => bulkSet(() => true, 'skip')}>
          Skip all
        </Button>
        <Button size="small" onClick={() => bulkSet((r) => r.classification === 'new', 'import')}>
          Import all new
        </Button>
        <Button
          size="small"
          onClick={() => bulkSet((r) => r.classification === 'conflict', 'skip')}
        >
          Skip all conflicts
        </Button>
        <Button
          size="small"
          danger
          onClick={() => bulkSet((r) => r.classification === 'conflict', 'overwrite')}
        >
          Overwrite all conflicts
        </Button>
        <Button
          size="small"
          onClick={() => bulkSet((r) => r.classification === 'conflict', 'duplicate')}
        >
          Duplicate all conflicts
        </Button>
        <Button size="small" type="link" onClick={() => setRows(null)}>
          Pick another file
        </Button>
      </Space>
      <Table
        dataSource={rows}
        columns={columns}
        rowKey="rowId"
        size="small"
        pagination={false}
        scroll={{ y: 320 }}
      />
      {invalidCount > 0 && (
        <Alert
          type="warning"
          showIcon
          title={`${invalidCount} row${invalidCount !== 1 ? 's' : ''} cannot be imported (invalid).`}
        />
      )}
      <Button
        type="primary"
        block
        icon={<UploadOutlined />}
        loading={applying}
        disabled={totalActionable === 0}
        onClick={handleApply}
      >
        Apply ({toImport} new · {toOverwrite} overwrite · {toDuplicate} duplicate)
      </Button>
    </Space>
  );
}

type ProviderRow = Pick<PreviewRow, 'provider'>;

export default function AccountTransferModal({ open, onClose }: AccountTransferModalProps) {
  return (
    <Modal
      open={open}
      onCancel={onClose}
      footer={null}
      title="Import / Export Accounts"
      centered
      width={780}
      destroyOnHidden
    >
      <Space orientation="vertical" size="middle" style={{ width: '100%' }}>
        <Alert
          type="warning"
          showIcon
          title="Export includes access keys, secrets, and tokens. Store the JSON securely."
        />
        <Tabs
          defaultActiveKey="export"
          items={[
            {
              key: 'export',
              label: (
                <span>
                  <DownloadOutlined /> Export
                </span>
              ),
              children: <ExportPanel />,
            },
            {
              key: 'import',
              label: (
                <span>
                  <UploadOutlined /> Import
                </span>
              ),
              children: <ImportPanel onApplied={onClose} />,
            },
          ]}
        />
      </Space>
    </Modal>
  );
}
