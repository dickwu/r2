'use client';

import { useMemo, useState, type ReactNode } from 'react';
import { App } from 'antd';
import {
  DownloadOutlined,
  UploadOutlined,
  FileAddOutlined,
  CheckOutlined,
  KeyOutlined,
  InboxOutlined,
  SwapOutlined,
  WarningOutlined,
} from '@ant-design/icons';
import { save, open as openDialog } from '@tauri-apps/plugin-dialog';
import { writeTextFile, readTextFile } from '@tauri-apps/plugin-fs';
import Modal from '@/app/components/ui/Modal';
import { useAccountStore, ProviderAccount } from '@/app/stores/accountStore';

const ACCOUNT_EXPORT_VERSION = 2;

type ProviderKey = 'r2' | 'aws' | 'minio' | 'rustfs';

const PROVIDER_ORDER: ProviderKey[] = ['r2', 'aws', 'minio', 'rustfs'];

const PROVIDER_LABELS: Record<ProviderKey, string> = {
  r2: 'Cloudflare R2',
  aws: 'AWS S3',
  minio: 'MinIO',
  rustfs: 'RustFS',
};

const PROVIDER_BADGE: Record<ProviderKey, string> = {
  r2: 'R2',
  aws: 'S3',
  minio: 'M',
  rustfs: 'RF',
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

export type TransferMode = 'export' | 'import';

interface AccountTransferModalProps {
  open: boolean;
  onClose: () => void;
  /** Which panel to show first. Defaults to export. */
  initialMode?: TransferMode;
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

/* ──────────────────────────────────────────────────────────────────
   Logic helpers (unchanged behavior — pure account transfer logic)
   ────────────────────────────────────────────────────────────────── */

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

/* ──────────────────────────────────────────────────────────────────
   Presentational primitives (design-token native)
   ────────────────────────────────────────────────────────────────── */

function ProviderBadge({ provider }: { provider: ProviderKey | 'unknown' }) {
  if (provider === 'unknown') {
    return <span className="pi pi-unknown">?</span>;
  }
  return <span className={`pi pi-${provider}`}>{PROVIDER_BADGE[provider]}</span>;
}

function CheckBox({
  checked,
  indeterminate,
  disabled,
  onChange,
}: {
  checked: boolean;
  indeterminate?: boolean;
  disabled?: boolean;
  onChange: (next: boolean) => void;
}) {
  return (
    <span
      role="checkbox"
      aria-checked={indeterminate ? 'mixed' : checked}
      aria-disabled={disabled || undefined}
      tabIndex={disabled ? -1 : 0}
      className={[
        'tx-check',
        (checked || indeterminate) && 'on',
        indeterminate && 'mixed',
        disabled && 'disabled',
      ]
        .filter(Boolean)
        .join(' ')}
      onClick={(e) => {
        e.stopPropagation();
        if (!disabled) onChange(!checked);
      }}
      onKeyDown={(e) => {
        if (disabled) return;
        if (e.key === ' ' || e.key === 'Enter') {
          e.preventDefault();
          onChange(!checked);
        }
      }}
    >
      {indeterminate ? (
        <span className="tx-check-dash" />
      ) : checked ? (
        <CheckOutlined style={{ fontSize: 10 }} />
      ) : null}
    </span>
  );
}

function StatusPill({ row }: { row: PreviewRow }) {
  if (row.classification === 'new') return <span className="tx-status is-new">New</span>;
  if (row.classification === 'conflict')
    return <span className="tx-status is-conflict">Conflict</span>;
  return (
    <span className="tx-status is-invalid" title={row.invalidReason}>
      Invalid
    </span>
  );
}

function SecurityNote() {
  return (
    <div className="tx-note">
      <KeyOutlined className="tx-note-icon" />
      <span>
        Exported files contain <strong>access keys, secrets, and API tokens in plain text</strong>.
        Store them somewhere safe and delete them once restored.
      </span>
    </div>
  );
}

/* ──────────────────────────────────────────────────────────────────
   Export panel
   ────────────────────────────────────────────────────────────────── */

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

  function setAllSelection(value: boolean) {
    setSelected(() => {
      if (!value) return new Set();
      return new Set(accounts.map(accountIdentity));
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
    return (
      <div className="tx-empty">
        <InboxOutlined className="tx-empty-icon" />
        <strong>No accounts to export</strong>
        <span>Add a storage provider first, then come back to back it up.</span>
      </div>
    );
  }

  const allSelected = selected.size === accounts.length;
  const someSelected = selected.size > 0 && !allSelected;
  const visibleProviders = PROVIDER_ORDER.filter((p) => grouped[p].length > 0);

  return (
    <div className="tx-panel">
      <div className="tx-toolbar">
        <button
          type="button"
          className="tx-toolbar-all"
          onClick={() => setAllSelection(!allSelected)}
        >
          <CheckBox
            checked={allSelected}
            indeterminate={someSelected}
            onChange={(v) => setAllSelection(v)}
          />
          <span>Select all accounts</span>
        </button>
        <span className="tx-toolbar-count">{selected.size} selected</span>
      </div>

      <div className="tx-groups">
        {visibleProviders.map((provider) => {
          const list = grouped[provider];
          const allIds = list.map(accountIdentity);
          const selectedCount = allIds.filter((id) => selected.has(id)).length;
          const groupAll = selectedCount === list.length;
          const groupSome = selectedCount > 0 && !groupAll;
          return (
            <section className="tx-group" key={provider}>
              <header
                className="tx-group-head"
                onClick={() => setProviderSelection(provider, !groupAll)}
              >
                <CheckBox
                  checked={groupAll}
                  indeterminate={groupSome}
                  onChange={(v) => setProviderSelection(provider, v)}
                />
                <ProviderBadge provider={provider} />
                <span className="tx-group-name">{PROVIDER_LABELS[provider]}</span>
                <span className="tx-group-count">
                  {selectedCount}/{list.length}
                </span>
              </header>
              <div className="tx-group-body">
                {list.map((acc) => {
                  const id = accountIdentity(acc);
                  return (
                    <div className="tx-acct" key={id} onClick={() => toggle(id)}>
                      <CheckBox checked={selected.has(id)} onChange={() => toggle(id)} />
                      <span className="tx-acct-name">{accountDisplayName(acc)}</span>
                      <span className="tx-acct-id">{accountIdentifierPreview(acc)}</span>
                    </div>
                  );
                })}
              </div>
            </section>
          );
        })}
      </div>

      <div className="tx-actionbar">
        <span className="tx-summary">
          {selected.size > 0 ? (
            <span>
              Exporting <b>{selected.size}</b> account{selected.size !== 1 ? 's' : ''}
            </span>
          ) : (
            <span>Choose accounts to include in the backup.</span>
          )}
        </span>
        <button
          type="button"
          className="btn btn-primary"
          disabled={selected.size === 0 || exporting}
          onClick={handleExport}
        >
          <DownloadOutlined style={{ fontSize: 12 }} />
          {exporting ? 'Exporting…' : 'Export to file'}
        </button>
      </div>
    </div>
  );
}

/* ──────────────────────────────────────────────────────────────────
   Import panel
   ────────────────────────────────────────────────────────────────── */

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
      <div className="tx-panel">
        <button type="button" className="tx-empty tx-empty-btn" onClick={handlePickFile}>
          <InboxOutlined className="tx-empty-icon" />
          <strong>Choose a backup file</strong>
          <span>Pick a previously exported JSON file to preview the accounts inside.</span>
          <span className="btn btn-primary tx-empty-cta">
            <FileAddOutlined style={{ fontSize: 12 }} />
            Browse files…
          </span>
        </button>
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

  return (
    <div className="tx-panel">
      <div className="tx-bulk">
        <button
          className="btn btn-sm"
          onClick={() => bulkSet((r) => r.classification === 'new', 'import')}
        >
          Import all new
        </button>
        <button
          className="btn btn-sm"
          onClick={() => bulkSet((r) => r.classification === 'conflict', 'overwrite')}
        >
          Overwrite conflicts
        </button>
        <button
          className="btn btn-sm"
          onClick={() => bulkSet((r) => r.classification === 'conflict', 'duplicate')}
        >
          Duplicate conflicts
        </button>
        <button className="btn btn-sm" onClick={() => bulkSet(() => true, 'skip')}>
          Skip all
        </button>
        <span className="tx-bulk-spacer" />
        <button className="btn btn-ghost btn-sm" onClick={() => setRows(null)}>
          Choose another file
        </button>
      </div>

      <div className="tx-preview">
        {rows.map((row) => {
          const isInvalid = row.classification === 'invalid';
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
            <div
              className={['tx-prow', isInvalid && 'is-invalid-row'].filter(Boolean).join(' ')}
              key={row.rowId}
            >
              <ProviderBadge provider={row.provider} />
              <div className="tx-prow-main">
                <span className="tx-prow-name">{row.name}</span>
                <span className="tx-prow-id">{row.identifierPreview}</span>
              </div>
              <StatusPill row={row} />
              {isInvalid ? (
                <span className="tx-prow-invalid">Cannot import</span>
              ) : (
                <select
                  className="tx-row-select"
                  value={row.action}
                  onChange={(e) => setRowAction(row.rowId, e.target.value as RowAction)}
                >
                  {options.map((o) => (
                    <option key={o.value} value={o.value}>
                      {o.label}
                    </option>
                  ))}
                </select>
              )}
            </div>
          );
        })}
      </div>

      {invalidCount > 0 && (
        <div className="tx-note tx-note-warn">
          <WarningOutlined className="tx-note-icon" />
          <span>
            {invalidCount} row{invalidCount !== 1 ? 's' : ''} cannot be imported because{' '}
            {invalidCount !== 1 ? 'they are' : 'it is'} missing required fields.
          </span>
        </div>
      )}

      <div className="tx-actionbar">
        <span className="tx-summary">
          <span>
            <b>{toImport}</b> new
          </span>
          <span>
            <b>{toOverwrite}</b> overwrite
          </span>
          <span>
            <b>{toDuplicate}</b> duplicate
          </span>
        </span>
        <button
          type="button"
          className="btn btn-primary"
          disabled={totalActionable === 0 || applying}
          onClick={handleApply}
        >
          <UploadOutlined style={{ fontSize: 12 }} />
          {applying
            ? 'Importing…'
            : `Import ${totalActionable} account${totalActionable !== 1 ? 's' : ''}`}
        </button>
      </div>
    </div>
  );
}

/* ──────────────────────────────────────────────────────────────────
   Modal shell
   ────────────────────────────────────────────────────────────────── */

export default function AccountTransferModal({
  open,
  onClose,
  initialMode = 'export',
}: AccountTransferModalProps) {
  const [mode, setMode] = useState<TransferMode>(initialMode);

  const tabs: Array<{ id: TransferMode; label: string; icon: ReactNode }> = [
    { id: 'export', label: 'Export', icon: <DownloadOutlined style={{ fontSize: 12 }} /> },
    { id: 'import', label: 'Import', icon: <UploadOutlined style={{ fontSize: 12 }} /> },
  ];

  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Backup & transfer"
      subtitle="Move your storage accounts between machines as a JSON file"
      icon={<SwapOutlined style={{ fontSize: 18 }} />}
      width={720}
    >
      <div className="tx-shell">
        <div className="segmented tx-toggle">
          {tabs.map((t) => (
            <button
              key={t.id}
              className={mode === t.id ? 'active' : undefined}
              onClick={() => setMode(t.id)}
            >
              {t.icon}
              {t.label}
            </button>
          ))}
        </div>

        <SecurityNote />

        {mode === 'export' ? <ExportPanel /> : <ImportPanel onApplied={onClose} />}
      </div>
    </Modal>
  );
}
