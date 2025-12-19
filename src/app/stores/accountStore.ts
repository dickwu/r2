import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';

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

export interface CurrentConfig {
  account_id: string;
  account_name: string | null;
  token_id: number;
  token_name: string | null;
  api_token: string;
  access_key_id: string;
  secret_access_key: string;
  bucket: string;
  public_domain: string | null;
}

// Legacy R2Config for compatibility with existing hooks
export interface R2Config {
  accountId: string;
  token: string;
  accessKeyId?: string;
  secretAccessKey?: string;
  bucket: string;
  buckets?: { name: string; publicDomain?: string }[];
  publicDomain?: string;
}

interface AccountStore {
  // State
  accounts: AccountWithTokens[];
  currentConfig: CurrentConfig | null;
  loading: boolean;
  initialized: boolean;

  // Actions
  initialize: () => Promise<void>;
  loadAccounts: () => Promise<void>;
  loadCurrentConfig: () => Promise<void>;

  // Selection
  selectBucket: (tokenId: number, bucketName: string) => Promise<void>;

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
    buckets: { name: string; public_domain?: string | null }[]
  ) => Promise<Bucket[]>;

  // Helpers
  hasAccounts: () => boolean;
  toR2Config: () => R2Config | null;
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
      const data = await invoke<AccountWithTokens[]>('get_all_accounts_with_tokens');
      set({ accounts: data });
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

  selectBucket: async (tokenId: number, bucketName: string) => {
    try {
      await invoke('set_current_token', { tokenId, bucketName });
      await get().loadCurrentConfig();
    } catch (e) {
      console.error('Failed to select bucket:', e);
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
      if (currentConfig?.account_id === id) {
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
        })),
      });
      await get().loadAccounts();
      return savedBuckets;
    } catch (e) {
      console.error('Failed to save buckets:', e);
      throw e;
    }
  },

  hasAccounts: () => {
    return get().accounts.length > 0;
  },

  toR2Config: () => {
    const { currentConfig } = get();
    if (!currentConfig) return null;

    return {
      accountId: currentConfig.account_id,
      token: currentConfig.api_token,
      accessKeyId: currentConfig.access_key_id,
      secretAccessKey: currentConfig.secret_access_key,
      bucket: currentConfig.bucket,
      publicDomain: currentConfig.public_domain || undefined,
    };
  },
}));
