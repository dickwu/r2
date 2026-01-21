import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import type { StorageConfig, StorageProvider } from '@/app/lib/r2cache';

// Types matching Rust structs
export interface Account {
  id: string;
  name: string | null;
  created_at: number;
  updated_at: number;
}

export interface Token {
  id: number;
  account_id: string;
  name: string | null;
  api_token: string;
  access_key_id: string;
  secret_access_key: string;
  created_at: number;
  updated_at: number;
}

export interface Bucket {
  id: number;
  token_id: number;
  name: string;
  public_domain: string | null;
  public_domain_scheme: string | null;
  created_at: number;
  updated_at: number;
}

export interface TokenWithBuckets {
  token: Token;
  buckets: Bucket[];
}

export interface AccountWithTokens {
  account: Account;
  tokens: TokenWithBuckets[];
}

export interface AwsAccount {
  id: string;
  name: string | null;
  access_key_id: string;
  secret_access_key: string;
  region: string;
  endpoint_scheme: string;
  endpoint_host: string | null;
  force_path_style: boolean;
  created_at: number;
  updated_at: number;
}

export interface AwsBucket {
  id: number;
  account_id: string;
  name: string;
  public_domain_scheme: string | null;
  public_domain_host: string | null;
  created_at: number;
  updated_at: number;
}

export interface AwsAccountWithBuckets {
  account: AwsAccount;
  buckets: AwsBucket[];
}

export interface MinioAccount {
  id: string;
  name: string | null;
  access_key_id: string;
  secret_access_key: string;
  endpoint_scheme: string;
  endpoint_host: string;
  force_path_style: boolean;
  created_at: number;
  updated_at: number;
}

export interface MinioBucket {
  id: number;
  account_id: string;
  name: string;
  public_domain_scheme: string | null;
  public_domain_host: string | null;
  created_at: number;
  updated_at: number;
}

export interface MinioAccountWithBuckets {
  account: MinioAccount;
  buckets: MinioBucket[];
}

export interface RustfsAccount {
  id: string;
  name: string | null;
  access_key_id: string;
  secret_access_key: string;
  endpoint_scheme: string;
  endpoint_host: string;
  force_path_style: boolean;
  created_at: number;
  updated_at: number;
}

export interface RustfsBucket {
  id: number;
  account_id: string;
  name: string;
  public_domain_scheme: string | null;
  public_domain_host: string | null;
  created_at: number;
  updated_at: number;
}

export interface RustfsAccountWithBuckets {
  account: RustfsAccount;
  buckets: RustfsBucket[];
}

export type ProviderAccount =
  | ({ provider: 'r2' } & AccountWithTokens)
  | ({ provider: 'aws' } & AwsAccountWithBuckets)
  | ({ provider: 'minio' } & MinioAccountWithBuckets)
  | ({ provider: 'rustfs' } & RustfsAccountWithBuckets);

export interface CurrentConfig {
  provider: StorageProvider;
  account_id: string;
  account_name: string | null;
  token_id?: number | null;
  token_name?: string | null;
  api_token?: string | null;
  access_key_id: string;
  secret_access_key: string;
  bucket: string;
  public_domain: string | null;
  public_domain_scheme?: string | null;
  region?: string | null;
  endpoint_scheme?: string | null;
  endpoint_host?: string | null;
  force_path_style?: boolean | null;
}

interface AccountStore {
  // State
  accounts: ProviderAccount[];
  currentConfig: CurrentConfig | null;
  loading: boolean;
  initialized: boolean;

  // Actions
  initialize: () => Promise<void>;
  loadAccounts: () => Promise<void>;
  loadCurrentConfig: () => Promise<void>;

  // Selection
  selectR2Bucket: (tokenId: number, bucketName: string) => Promise<void>;
  selectAwsBucket: (accountId: string, bucketName: string) => Promise<void>;
  selectMinioBucket: (accountId: string, bucketName: string) => Promise<void>;
  selectRustfsBucket: (accountId: string, bucketName: string) => Promise<void>;

  // Account CRUD
  createAccount: (id: string, name?: string) => Promise<Account>;
  updateAccount: (id: string, name?: string) => Promise<void>;
  deleteAccount: (id: string) => Promise<void>;

  // Token CRUD
  createToken: (input: {
    account_id: string;
    name?: string;
    api_token: string;
    access_key_id: string;
    secret_access_key: string;
  }) => Promise<Token>;
  updateToken: (input: {
    id: number;
    name?: string;
    api_token: string;
    access_key_id: string;
    secret_access_key: string;
  }) => Promise<void>;
  deleteToken: (id: number) => Promise<void>;

  // Bucket CRUD
  saveBuckets: (
    tokenId: number,
    buckets: { name: string; public_domain?: string | null; public_domain_scheme?: string | null }[]
  ) => Promise<Bucket[]>;

  // AWS Account CRUD
  createAwsAccount: (input: {
    name?: string;
    access_key_id: string;
    secret_access_key: string;
    region: string;
    endpoint_scheme?: string | null;
    endpoint_host?: string | null;
    force_path_style: boolean;
  }) => Promise<AwsAccount>;
  updateAwsAccount: (input: {
    id: string;
    name?: string;
    access_key_id: string;
    secret_access_key: string;
    region: string;
    endpoint_scheme?: string | null;
    endpoint_host?: string | null;
    force_path_style: boolean;
  }) => Promise<void>;
  deleteAwsAccount: (id: string) => Promise<void>;
  saveAwsBuckets: (
    accountId: string,
    buckets: {
      name: string;
      public_domain_scheme?: string | null;
      public_domain_host?: string | null;
    }[]
  ) => Promise<AwsBucket[]>;

  // MinIO Account CRUD
  createMinioAccount: (input: {
    name?: string;
    access_key_id: string;
    secret_access_key: string;
    endpoint_scheme: string;
    endpoint_host: string;
    force_path_style: boolean;
  }) => Promise<MinioAccount>;
  updateMinioAccount: (input: {
    id: string;
    name?: string;
    access_key_id: string;
    secret_access_key: string;
    endpoint_scheme: string;
    endpoint_host: string;
    force_path_style: boolean;
  }) => Promise<void>;
  deleteMinioAccount: (id: string) => Promise<void>;
  saveMinioBuckets: (
    accountId: string,
    buckets: {
      name: string;
      public_domain_scheme?: string | null;
      public_domain_host?: string | null;
    }[]
  ) => Promise<MinioBucket[]>;

  // RustFS Account CRUD
  createRustfsAccount: (input: {
    name?: string;
    access_key_id: string;
    secret_access_key: string;
    endpoint_scheme: string;
    endpoint_host: string;
  }) => Promise<RustfsAccount>;
  updateRustfsAccount: (input: {
    id: string;
    name?: string;
    access_key_id: string;
    secret_access_key: string;
    endpoint_scheme: string;
    endpoint_host: string;
  }) => Promise<void>;
  deleteRustfsAccount: (id: string) => Promise<void>;
  saveRustfsBuckets: (
    accountId: string,
    buckets: {
      name: string;
      public_domain_scheme?: string | null;
      public_domain_host?: string | null;
    }[]
  ) => Promise<RustfsBucket[]>;

  // Helpers
  hasAccounts: () => boolean;
  toStorageConfig: () => StorageConfig | null;
}

export const useAccountStore = create<AccountStore>((set, get) => ({
  accounts: [],
  currentConfig: null,
  loading: true,
  initialized: false,

  initialize: async () => {
    const { loadAccounts, loadCurrentConfig } = get();
    set({ loading: true });
    try {
      await loadAccounts();
      await loadCurrentConfig();
      set({ initialized: true });
    } catch (e) {
      console.error('Failed to initialize account store:', e);
    } finally {
      set({ loading: false });
    }
  },

  loadAccounts: async () => {
    try {
      const [r2Accounts, awsAccounts, minioAccounts, rustfsAccounts] = await Promise.all([
        invoke<AccountWithTokens[]>('get_all_accounts_with_tokens'),
        invoke<AwsAccountWithBuckets[]>('get_all_aws_accounts_with_buckets'),
        invoke<MinioAccountWithBuckets[]>('get_all_minio_accounts_with_buckets'),
        invoke<RustfsAccountWithBuckets[]>('get_all_rustfs_accounts_with_buckets'),
      ]);

      const combined: ProviderAccount[] = [
        ...r2Accounts.map((account) => ({ provider: 'r2' as const, ...account })),
        ...awsAccounts.map((account) => ({ provider: 'aws' as const, ...account })),
        ...minioAccounts.map((account) => ({ provider: 'minio' as const, ...account })),
        ...rustfsAccounts.map((account) => ({ provider: 'rustfs' as const, ...account })),
      ];

      set({ accounts: combined });
    } catch (e) {
      console.error('Failed to load accounts:', e);
      throw e;
    }
  },

  loadCurrentConfig: async () => {
    try {
      const config = await invoke<CurrentConfig | null>('get_current_config');
      set({ currentConfig: config });
    } catch (e) {
      console.error('Failed to load current config:', e);
      throw e;
    }
  },

  selectR2Bucket: async (tokenId: number, bucketName: string) => {
    try {
      await invoke('set_current_token', { tokenId, bucketName });
      await get().loadCurrentConfig();
    } catch (e) {
      console.error('Failed to select R2 bucket:', e);
      throw e;
    }
  },

  selectAwsBucket: async (accountId: string, bucketName: string) => {
    try {
      await invoke('set_current_aws_bucket', { accountId, bucketName });
      await get().loadCurrentConfig();
    } catch (e) {
      console.error('Failed to select AWS bucket:', e);
      throw e;
    }
  },

  selectMinioBucket: async (accountId: string, bucketName: string) => {
    try {
      await invoke('set_current_minio_bucket', { accountId, bucketName });
      await get().loadCurrentConfig();
    } catch (e) {
      console.error('Failed to select MinIO bucket:', e);
      throw e;
    }
  },

  selectRustfsBucket: async (accountId: string, bucketName: string) => {
    try {
      await invoke('set_current_rustfs_bucket', { accountId, bucketName });
      await get().loadCurrentConfig();
    } catch (e) {
      console.error('Failed to select RustFS bucket:', e);
      throw e;
    }
  },

  createAccount: async (id: string, name?: string) => {
    try {
      const account = await invoke<Account>('create_account', {
        id,
        name: name || null,
      });
      await get().loadAccounts();
      return account;
    } catch (e) {
      console.error('Failed to create account:', e);
      throw e;
    }
  },

  updateAccount: async (id: string, name?: string) => {
    try {
      await invoke('update_account', {
        id,
        name: name || null,
      });
      await get().loadAccounts();
      await get().loadCurrentConfig();
    } catch (e) {
      console.error('Failed to update account:', e);
      throw e;
    }
  },

  deleteAccount: async (id: string) => {
    const { currentConfig, loadAccounts, loadCurrentConfig } = get();
    try {
      await invoke('delete_account', { id });
      await loadAccounts();
      // If deleted current account, reload config
      if (currentConfig?.provider === 'r2' && currentConfig.account_id === id) {
        await loadCurrentConfig();
      }
    } catch (e) {
      console.error('Failed to delete account:', e);
      throw e;
    }
  },

  createToken: async (input) => {
    try {
      const token = await invoke<Token>('create_token', {
        input: {
          account_id: input.account_id,
          name: input.name || null,
          api_token: input.api_token,
          access_key_id: input.access_key_id,
          secret_access_key: input.secret_access_key,
        },
      });
      await get().loadAccounts();
      return token;
    } catch (e) {
      console.error('Failed to create token:', e);
      throw e;
    }
  },

  updateToken: async (input) => {
    try {
      await invoke('update_token', {
        input: {
          id: input.id,
          name: input.name || null,
          api_token: input.api_token,
          access_key_id: input.access_key_id,
          secret_access_key: input.secret_access_key,
        },
      });
      await get().loadAccounts();
      await get().loadCurrentConfig();
    } catch (e) {
      console.error('Failed to update token:', e);
      throw e;
    }
  },

  deleteToken: async (id: number) => {
    const { currentConfig, loadAccounts, loadCurrentConfig } = get();
    try {
      await invoke('delete_token', { id });
      await loadAccounts();
      // If deleted current token, reload config
      if (currentConfig?.token_id === id) {
        await loadCurrentConfig();
      }
    } catch (e) {
      console.error('Failed to delete token:', e);
      throw e;
    }
  },

  saveBuckets: async (tokenId: number, buckets) => {
    try {
      const savedBuckets = await invoke<Bucket[]>('save_buckets', {
        tokenId,
        buckets: buckets.map((b) => ({
          name: b.name,
          public_domain: b.public_domain || null,
          public_domain_scheme: b.public_domain_scheme || null,
        })),
      });
      await get().loadAccounts();
      return savedBuckets;
    } catch (e) {
      console.error('Failed to save buckets:', e);
      throw e;
    }
  },

  createAwsAccount: async (input) => {
    try {
      const account = await invoke<AwsAccount>('create_aws_account', {
        input: {
          name: input.name || null,
          access_key_id: input.access_key_id,
          secret_access_key: input.secret_access_key,
          region: input.region,
          endpoint_scheme: input.endpoint_scheme ?? null,
          endpoint_host: input.endpoint_host ?? null,
          force_path_style: input.force_path_style,
        },
      });
      await get().loadAccounts();
      return account;
    } catch (e) {
      console.error('Failed to create AWS account:', e);
      throw e;
    }
  },

  updateAwsAccount: async (input) => {
    try {
      await invoke('update_aws_account', {
        input: {
          id: input.id,
          name: input.name || null,
          access_key_id: input.access_key_id,
          secret_access_key: input.secret_access_key,
          region: input.region,
          endpoint_scheme: input.endpoint_scheme ?? null,
          endpoint_host: input.endpoint_host ?? null,
          force_path_style: input.force_path_style,
        },
      });
      await get().loadAccounts();
      await get().loadCurrentConfig();
    } catch (e) {
      console.error('Failed to update AWS account:', e);
      throw e;
    }
  },

  deleteAwsAccount: async (id: string) => {
    const { currentConfig, loadAccounts, loadCurrentConfig } = get();
    try {
      await invoke('delete_aws_account', { id });
      await loadAccounts();
      if (currentConfig?.provider === 'aws' && currentConfig.account_id === id) {
        await loadCurrentConfig();
      }
    } catch (e) {
      console.error('Failed to delete AWS account:', e);
      throw e;
    }
  },

  saveAwsBuckets: async (accountId, buckets) => {
    try {
      const savedBuckets = await invoke<AwsBucket[]>('save_aws_bucket_configs', {
        accountId,
        buckets: buckets.map((b) => ({
          name: b.name,
          public_domain_scheme: b.public_domain_scheme ?? null,
          public_domain_host: b.public_domain_host ?? null,
        })),
      });
      await get().loadAccounts();
      return savedBuckets;
    } catch (e) {
      console.error('Failed to save AWS buckets:', e);
      throw e;
    }
  },

  createMinioAccount: async (input) => {
    try {
      const account = await invoke<MinioAccount>('create_minio_account', {
        input: {
          name: input.name || null,
          access_key_id: input.access_key_id,
          secret_access_key: input.secret_access_key,
          endpoint_scheme: input.endpoint_scheme,
          endpoint_host: input.endpoint_host,
          force_path_style: input.force_path_style,
        },
      });
      await get().loadAccounts();
      return account;
    } catch (e) {
      console.error('Failed to create MinIO account:', e);
      throw e;
    }
  },

  updateMinioAccount: async (input) => {
    try {
      await invoke('update_minio_account', {
        input: {
          id: input.id,
          name: input.name || null,
          access_key_id: input.access_key_id,
          secret_access_key: input.secret_access_key,
          endpoint_scheme: input.endpoint_scheme,
          endpoint_host: input.endpoint_host,
          force_path_style: input.force_path_style,
        },
      });
      await get().loadAccounts();
      await get().loadCurrentConfig();
    } catch (e) {
      console.error('Failed to update MinIO account:', e);
      throw e;
    }
  },

  deleteMinioAccount: async (id: string) => {
    const { currentConfig, loadAccounts, loadCurrentConfig } = get();
    try {
      await invoke('delete_minio_account', { id });
      await loadAccounts();
      if (currentConfig?.provider === 'minio' && currentConfig.account_id === id) {
        await loadCurrentConfig();
      }
    } catch (e) {
      console.error('Failed to delete MinIO account:', e);
      throw e;
    }
  },

  saveMinioBuckets: async (accountId, buckets) => {
    try {
      const savedBuckets = await invoke<MinioBucket[]>('save_minio_bucket_configs', {
        accountId,
        buckets: buckets.map((b) => ({
          name: b.name,
          public_domain_scheme: b.public_domain_scheme ?? null,
          public_domain_host: b.public_domain_host ?? null,
        })),
      });
      await get().loadAccounts();
      return savedBuckets;
    } catch (e) {
      console.error('Failed to save MinIO buckets:', e);
      throw e;
    }
  },

  createRustfsAccount: async (input) => {
    try {
      const account = await invoke<RustfsAccount>('create_rustfs_account', {
        input: {
          name: input.name || null,
          access_key_id: input.access_key_id,
          secret_access_key: input.secret_access_key,
          endpoint_scheme: input.endpoint_scheme,
          endpoint_host: input.endpoint_host,
        },
      });
      await get().loadAccounts();
      return account;
    } catch (e) {
      console.error('Failed to create RustFS account:', e);
      throw e;
    }
  },

  updateRustfsAccount: async (input) => {
    try {
      await invoke('update_rustfs_account', {
        input: {
          id: input.id,
          name: input.name || null,
          access_key_id: input.access_key_id,
          secret_access_key: input.secret_access_key,
          endpoint_scheme: input.endpoint_scheme,
          endpoint_host: input.endpoint_host,
        },
      });
      await get().loadAccounts();
      await get().loadCurrentConfig();
    } catch (e) {
      console.error('Failed to update RustFS account:', e);
      throw e;
    }
  },

  deleteRustfsAccount: async (id: string) => {
    const { currentConfig, loadAccounts, loadCurrentConfig } = get();
    try {
      await invoke('delete_rustfs_account', { id });
      await loadAccounts();
      if (currentConfig?.provider === 'rustfs' && currentConfig.account_id === id) {
        await loadCurrentConfig();
      }
    } catch (e) {
      console.error('Failed to delete RustFS account:', e);
      throw e;
    }
  },

  saveRustfsBuckets: async (accountId, buckets) => {
    try {
      const savedBuckets = await invoke<RustfsBucket[]>('save_rustfs_bucket_configs', {
        accountId,
        buckets: buckets.map((b) => ({
          name: b.name,
          public_domain_scheme: b.public_domain_scheme ?? null,
          public_domain_host: b.public_domain_host ?? null,
        })),
      });
      await get().loadAccounts();
      return savedBuckets;
    } catch (e) {
      console.error('Failed to save RustFS buckets:', e);
      throw e;
    }
  },

  hasAccounts: () => {
    return get().accounts.length > 0;
  },

  toStorageConfig: () => {
    const { currentConfig } = get();
    if (!currentConfig) return null;

    if (currentConfig.provider === 'r2') {
      return {
        provider: 'r2',
        accountId: currentConfig.account_id,
        token: currentConfig.api_token || undefined,
        accessKeyId: currentConfig.access_key_id,
        secretAccessKey: currentConfig.secret_access_key,
        bucket: currentConfig.bucket,
        publicDomain: currentConfig.public_domain || undefined,
        publicDomainScheme: currentConfig.public_domain_scheme || undefined,
      };
    }

    if (currentConfig.provider === 'aws') {
      if (!currentConfig.region) return null;
      return {
        provider: 'aws',
        accountId: currentConfig.account_id,
        accessKeyId: currentConfig.access_key_id,
        secretAccessKey: currentConfig.secret_access_key,
        region: currentConfig.region,
        endpointScheme: currentConfig.endpoint_scheme || undefined,
        endpointHost: currentConfig.endpoint_host || undefined,
        forcePathStyle: currentConfig.force_path_style ?? false,
        bucket: currentConfig.bucket,
        publicDomain: currentConfig.public_domain || undefined,
        publicDomainScheme: currentConfig.public_domain_scheme || undefined,
      };
    }

    if (currentConfig.provider === 'minio') {
      if (!currentConfig.endpoint_scheme || !currentConfig.endpoint_host) {
        return null;
      }
      return {
        provider: 'minio',
        accountId: currentConfig.account_id,
        accessKeyId: currentConfig.access_key_id,
        secretAccessKey: currentConfig.secret_access_key,
        endpointScheme: currentConfig.endpoint_scheme,
        endpointHost: currentConfig.endpoint_host,
        forcePathStyle: currentConfig.force_path_style ?? false,
        bucket: currentConfig.bucket,
      };
    }

    if (currentConfig.provider === 'rustfs') {
      if (!currentConfig.endpoint_scheme || !currentConfig.endpoint_host) {
        return null;
      }
      return {
        provider: 'rustfs',
        accountId: currentConfig.account_id,
        accessKeyId: currentConfig.access_key_id,
        secretAccessKey: currentConfig.secret_access_key,
        endpointScheme: currentConfig.endpoint_scheme,
        endpointHost: currentConfig.endpoint_host,
        forcePathStyle: true,
        bucket: currentConfig.bucket,
      };
    }

    return null;
  },
}));
