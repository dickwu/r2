import type { ProviderAccount } from '@/app/stores/accountStore';
import type { MoveTask } from '@/app/stores/moveStore';

export interface MoveConfigInput {
  provider: string;
  account_id: string;
  bucket: string;
  access_key_id: string;
  secret_access_key: string;
  region?: string | null;
  endpoint_scheme?: string | null;
  endpoint_host?: string | null;
  force_path_style?: boolean | null;
}

export function buildMoveConfigFromAccounts(
  provider: string,
  accountId: string,
  bucket: string,
  accounts: ProviderAccount[]
): MoveConfigInput | null {
  const accountEntry = accounts.find(
    (account) => account.provider === provider && account.account.id === accountId
  );
  if (!accountEntry) return null;

  if (accountEntry.provider === 'r2') {
    const tokenEntry = accountEntry.tokens.find((token) =>
      token.buckets.some((item) => item.name === bucket)
    );
    if (!tokenEntry) return null;
    return {
      provider: 'r2',
      account_id: accountEntry.account.id,
      bucket,
      access_key_id: tokenEntry.token.access_key_id,
      secret_access_key: tokenEntry.token.secret_access_key,
      region: null,
      endpoint_scheme: null,
      endpoint_host: null,
      force_path_style: null,
    };
  }

  if (accountEntry.provider === 'aws') {
    return {
      provider: 'aws',
      account_id: accountEntry.account.id,
      bucket,
      access_key_id: accountEntry.account.access_key_id,
      secret_access_key: accountEntry.account.secret_access_key,
      region: accountEntry.account.region,
      endpoint_scheme: accountEntry.account.endpoint_scheme,
      endpoint_host: accountEntry.account.endpoint_host,
      force_path_style: accountEntry.account.force_path_style,
    };
  }

  if (accountEntry.provider === 'minio') {
    return {
      provider: 'minio',
      account_id: accountEntry.account.id,
      bucket,
      access_key_id: accountEntry.account.access_key_id,
      secret_access_key: accountEntry.account.secret_access_key,
      endpoint_scheme: accountEntry.account.endpoint_scheme,
      endpoint_host: accountEntry.account.endpoint_host,
      force_path_style: accountEntry.account.force_path_style,
    };
  }

  if (accountEntry.provider === 'rustfs') {
    return {
      provider: 'rustfs',
      account_id: accountEntry.account.id,
      bucket,
      access_key_id: accountEntry.account.access_key_id,
      secret_access_key: accountEntry.account.secret_access_key,
      endpoint_scheme: accountEntry.account.endpoint_scheme,
      endpoint_host: accountEntry.account.endpoint_host,
      force_path_style: true,
    };
  }

  return null;
}

export function buildDestinationConfigFromAccounts(
  task: MoveTask,
  accounts: ProviderAccount[]
): MoveConfigInput | null {
  return buildMoveConfigFromAccounts(
    task.destProvider,
    task.destAccountId,
    task.destBucket,
    accounts
  );
}

export function buildSourceConfigFromAccounts(
  task: MoveTask,
  accounts: ProviderAccount[]
): MoveConfigInput | null {
  return buildMoveConfigFromAccounts(
    task.sourceProvider,
    task.sourceAccountId,
    task.sourceBucket,
    accounts
  );
}
