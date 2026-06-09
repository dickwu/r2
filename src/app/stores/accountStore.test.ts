import { describe, expect, test, mock, beforeEach } from 'bun:test';

/**
 * Regression test for the "public domain only applies after restart" bug.
 *
 * Editing a bucket's public domain funnels through saveBuckets()/saveAwsBuckets()/
 * etc., which previously refreshed only `accounts` and never `currentConfig`.
 * Because the live StorageConfig — and therefore every public object URL — is
 * derived from `currentConfig` (see page.tsx `useMemo([currentConfig])`), a
 * domain change on the *active* bucket stayed invisible until the app
 * re-initialized on refresh/restart. The save actions must now reload
 * `currentConfig` whenever the saved bucket belongs to the active selection.
 */

type InvokeArgs = Record<string, unknown> | undefined;
type InvokeFn = (cmd: string, args?: InvokeArgs) => Promise<unknown>;

// Reassigned per test; the module mock below closes over this binding by
// reference, so each test can install its own fake backend.
let handleInvoke: InvokeFn = async () => undefined;

mock.module('@tauri-apps/api/core', () => ({
  invoke: (cmd: string, args?: InvokeArgs) => handleInvoke(cmd, args),
}));

// Import after the mock is registered so the store binds to the fake invoke.
const { useAccountStore } = await import('./accountStore');

const EMPTY_ACCOUNT_LISTS = new Set([
  'get_all_accounts_with_tokens',
  'get_all_aws_accounts_with_buckets',
  'get_all_minio_accounts_with_buckets',
  'get_all_rustfs_accounts_with_buckets',
]);

function r2Config(overrides: Record<string, unknown> = {}) {
  return {
    provider: 'r2' as const,
    account_id: 'acc-1',
    account_name: 'Acct',
    token_id: 1,
    api_token: 't',
    access_key_id: 'ak',
    secret_access_key: 'sk',
    bucket: 'mybucket',
    public_domain: null as string | null,
    public_domain_scheme: null as string | null,
    ...overrides,
  };
}

function awsConfig(overrides: Record<string, unknown> = {}) {
  return {
    provider: 'aws' as const,
    account_id: 'aws-1',
    account_name: 'AWS',
    access_key_id: 'ak',
    secret_access_key: 'sk',
    bucket: 'b',
    region: 'us-east-1',
    public_domain: null as string | null,
    public_domain_scheme: null as string | null,
    ...overrides,
  };
}

beforeEach(() => {
  handleInvoke = async () => undefined;
  useAccountStore.setState({ accounts: [], currentConfig: null });
});

describe('saveBuckets (R2) refreshes the live config', () => {
  test('reloads currentConfig when the saved token is the active selection', async () => {
    const calls: string[] = [];
    handleInvoke = async (cmd) => {
      calls.push(cmd);
      if (EMPTY_ACCOUNT_LISTS.has(cmd)) return [];
      if (cmd === 'save_buckets') return [];
      // Simulate the backend now reading the freshly-saved domain from the DB.
      if (cmd === 'get_current_config') {
        return r2Config({ public_domain: 'cdn.example.com', public_domain_scheme: 'https' });
      }
      return undefined;
    };

    useAccountStore.setState({ currentConfig: r2Config() });

    await useAccountStore
      .getState()
      .saveBuckets(1, [
        { name: 'mybucket', public_domain: 'cdn.example.com', public_domain_scheme: 'https' },
      ]);

    expect(calls).toContain('get_current_config');
    // The change is visible immediately, without a restart.
    expect(useAccountStore.getState().currentConfig?.public_domain).toBe('cdn.example.com');
    expect(useAccountStore.getState().toStorageConfig()?.publicDomain).toBe('cdn.example.com');
  });

  test('does not reload currentConfig when saving a different token', async () => {
    const calls: string[] = [];
    handleInvoke = async (cmd) => {
      calls.push(cmd);
      if (EMPTY_ACCOUNT_LISTS.has(cmd)) return [];
      if (cmd === 'save_buckets') return [];
      if (cmd === 'get_current_config') return r2Config({ public_domain: 'cdn.example.com' });
      return undefined;
    };

    useAccountStore.setState({ currentConfig: r2Config({ token_id: 1 }) });

    await useAccountStore
      .getState()
      .saveBuckets(2, [
        { name: 'other', public_domain: 'cdn.example.com', public_domain_scheme: 'https' },
      ]);

    expect(calls).not.toContain('get_current_config');
    expect(useAccountStore.getState().currentConfig?.public_domain).toBeNull();
  });
});

describe('saveAwsBuckets refreshes the live config', () => {
  test('reloads currentConfig when the saved account is the active aws selection', async () => {
    const calls: string[] = [];
    handleInvoke = async (cmd) => {
      calls.push(cmd);
      if (EMPTY_ACCOUNT_LISTS.has(cmd)) return [];
      if (cmd === 'save_aws_bucket_configs') return [];
      if (cmd === 'get_current_config') {
        return awsConfig({ public_domain: 'cdn.aws.example.com', public_domain_scheme: 'https' });
      }
      return undefined;
    };

    useAccountStore.setState({ currentConfig: awsConfig() });

    await useAccountStore
      .getState()
      .saveAwsBuckets('aws-1', [
        { name: 'b', public_domain_host: 'cdn.aws.example.com', public_domain_scheme: 'https' },
      ]);

    expect(calls).toContain('get_current_config');
    expect(useAccountStore.getState().toStorageConfig()?.publicDomain).toBe('cdn.aws.example.com');
  });

  test('does not reload currentConfig when the active selection is a different provider', async () => {
    const calls: string[] = [];
    handleInvoke = async (cmd) => {
      calls.push(cmd);
      if (EMPTY_ACCOUNT_LISTS.has(cmd)) return [];
      if (cmd === 'save_aws_bucket_configs') return [];
      if (cmd === 'get_current_config') return awsConfig({ public_domain: 'cdn.aws.example.com' });
      return undefined;
    };

    // Active selection is R2, but we save an AWS account's buckets.
    useAccountStore.setState({ currentConfig: r2Config() });

    await useAccountStore
      .getState()
      .saveAwsBuckets('aws-1', [
        { name: 'b', public_domain_host: 'cdn.aws.example.com', public_domain_scheme: 'https' },
      ]);

    expect(calls).not.toContain('get_current_config');
  });
});

describe('is_public flows through saving and the live config', () => {
  test('saveBuckets (R2) forwards is_public and public_path_prefix to the backend', async () => {
    let savedArgs: Record<string, unknown> | undefined;
    handleInvoke = async (cmd, args) => {
      if (EMPTY_ACCOUNT_LISTS.has(cmd)) return [];
      if (cmd === 'save_buckets') {
        savedArgs = args;
        return [];
      }
      if (cmd === 'get_current_config') return r2Config();
      return undefined;
    };

    useAccountStore.setState({ currentConfig: r2Config({ token_id: 7 }) });

    await useAccountStore.getState().saveBuckets(7, [
      {
        name: 'mybucket',
        public_domain: 'cdn.example.com',
        public_domain_scheme: 'https',
        is_public: true,
        public_path_prefix: 'assets',
      },
    ]);

    const sent = (savedArgs as { buckets: Array<Record<string, unknown>> }).buckets[0];
    expect(sent.is_public).toBe(true);
    expect(sent.public_path_prefix).toBe('assets');
  });

  test('saveAwsBuckets forwards is_public to the backend', async () => {
    let savedArgs: Record<string, unknown> | undefined;
    handleInvoke = async (cmd, args) => {
      if (EMPTY_ACCOUNT_LISTS.has(cmd)) return [];
      if (cmd === 'save_aws_bucket_configs') {
        savedArgs = args;
        return [];
      }
      if (cmd === 'get_current_config') return awsConfig();
      return undefined;
    };

    useAccountStore.setState({ currentConfig: awsConfig() });

    await useAccountStore
      .getState()
      .saveAwsBuckets('aws-1', [{ name: 'b', is_public: true }]);

    const sent = (savedArgs as { buckets: Array<Record<string, unknown>> }).buckets[0];
    expect(sent.is_public).toBe(true);
  });

  test('toStorageConfig maps is_public + public_path_prefix into the live StorageConfig (R2)', () => {
    useAccountStore.setState({
      currentConfig: r2Config({
        public_domain: 'cdn.example.com',
        is_public: true,
        public_path_prefix: 'assets',
      }),
    });
    const cfg = useAccountStore.getState().toStorageConfig();
    expect(cfg?.isPublic).toBe(true);
    expect(cfg?.publicPathPrefix).toBe('assets');
  });

  test('toStorageConfig defaults isPublic to false when the flag is absent', () => {
    useAccountStore.setState({ currentConfig: r2Config() });
    const cfg = useAccountStore.getState().toStorageConfig();
    expect(cfg?.isPublic).toBe(false);
  });

  test('toStorageConfig maps is_public for S3-family providers (AWS)', () => {
    useAccountStore.setState({ currentConfig: awsConfig({ is_public: true }) });
    const cfg = useAccountStore.getState().toStorageConfig();
    expect(cfg?.isPublic).toBe(true);
  });
});
