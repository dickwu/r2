'use client';

import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Form, Input, Button, Modal, App, Space, Divider, Tag, Select, Switch } from 'antd';
import {
  ReloadOutlined,
  PlusOutlined,
  DeleteOutlined,
  CheckOutlined,
  UserOutlined,
  KeyOutlined,
} from '@ant-design/icons';
import { listBuckets, StorageProvider } from '../lib/r2cache';
import {
  useAccountStore,
  Account,
  Token,
  Bucket,
  AwsAccount,
  MinioAccount,
  ProviderAccount,
  AwsBucket,
  MinioBucket,
  RustfsBucket,
} from '../stores/accountStore';

export interface BucketConfig {
  name: string;
  publicDomainHost?: string;
  publicDomainScheme?: string;
}

interface FormValues {
  accountId: string;
  accountName?: string;
  tokenName?: string;
  apiToken: string;
  accessKeyId: string;
  secretAccessKey: string;
  region?: string;
  endpointScheme?: string;
  endpointHost?: string;
  forcePathStyle?: boolean;
  selectedBucket?: string;
}

export type ModalMode = 'add-account' | 'edit-account' | 'add-token' | 'edit-token';

interface ConfigModalProps {
  open: boolean;
  onClose: () => void;
  mode: ModalMode;
  editAccount?: ProviderAccount | null;
  editToken?: Token | null;
  parentAccountId?: string; // For add-token mode
}

export default function ConfigModal({
  open,
  onClose,
  mode,
  editAccount,
  editToken,
  parentAccountId,
}: ConfigModalProps) {
  const [saving, setSaving] = useState(false);
  const [loadingBuckets, setLoadingBuckets] = useState(false);
  const [buckets, setBuckets] = useState<BucketConfig[]>([]);
  const [addingBucket, setAddingBucket] = useState(false);
  const [newBucketName, setNewBucketName] = useState('');
  const [provider, setProvider] = useState<StorageProvider>('r2');
  const [form] = Form.useForm<FormValues>();
  const selectedBucket = Form.useWatch('selectedBucket', form);
  const { message } = App.useApp();

  // Use Zustand store
  const accounts = useAccountStore((state) => state.accounts);
  const createAccount = useAccountStore((state) => state.createAccount);
  const updateAccountFn = useAccountStore((state) => state.updateAccount);
  const createToken = useAccountStore((state) => state.createToken);
  const updateTokenFn = useAccountStore((state) => state.updateToken);
  const saveBuckets = useAccountStore((state) => state.saveBuckets);
  const selectR2Bucket = useAccountStore((state) => state.selectR2Bucket);
  const createAwsAccount = useAccountStore((state) => state.createAwsAccount);
  const updateAwsAccount = useAccountStore((state) => state.updateAwsAccount);
  const saveAwsBuckets = useAccountStore((state) => state.saveAwsBuckets);
  const selectAwsBucket = useAccountStore((state) => state.selectAwsBucket);
  const createMinioAccount = useAccountStore((state) => state.createMinioAccount);
  const updateMinioAccount = useAccountStore((state) => state.updateMinioAccount);
  const saveMinioBuckets = useAccountStore((state) => state.saveMinioBuckets);
  const selectMinioBucket = useAccountStore((state) => state.selectMinioBucket);
  const createRustfsAccount = useAccountStore((state) => state.createRustfsAccount);
  const updateRustfsAccount = useAccountStore((state) => state.updateRustfsAccount);
  const saveRustfsBuckets = useAccountStore((state) => state.saveRustfsBuckets);
  const selectRustfsBucket = useAccountStore((state) => state.selectRustfsBucket);

  // Determine if we're in account mode or token mode
  const isAccountMode = mode === 'add-account' || mode === 'edit-account';
  const isEditMode = mode === 'edit-account' || mode === 'edit-token';
  const isR2Provider = provider === 'r2';
  const showDomainSettings = provider === 'r2' || provider === 'aws';

  useEffect(() => {
    if (!open) {
      form.resetFields();
      setBuckets([]);
      return;
    }

    form.resetFields();
    setBuckets([]);

    if (mode === 'edit-account' && editAccount) {
      setProvider(editAccount.provider);
      if (editAccount.provider === 'r2') {
        form.setFieldsValue({
          accountId: editAccount.account.id,
          accountName: editAccount.account.name || '',
        });
      } else if (editAccount.provider === 'aws') {
        loadAwsBuckets(editAccount.account.id);
        form.setFieldsValue({
          accountName: editAccount.account.name || '',
          accessKeyId: editAccount.account.access_key_id,
          secretAccessKey: editAccount.account.secret_access_key,
          region: editAccount.account.region,
          endpointScheme: editAccount.account.endpoint_scheme,
          endpointHost: editAccount.account.endpoint_host || '',
          forcePathStyle: editAccount.account.force_path_style,
        });
      } else if (editAccount.provider === 'minio') {
        loadMinioBuckets(editAccount.account.id);
        form.setFieldsValue({
          accountName: editAccount.account.name || '',
          accessKeyId: editAccount.account.access_key_id,
          secretAccessKey: editAccount.account.secret_access_key,
          endpointScheme: editAccount.account.endpoint_scheme,
          endpointHost: editAccount.account.endpoint_host,
          forcePathStyle: editAccount.account.force_path_style,
        });
      } else if (editAccount.provider === 'rustfs') {
        loadRustfsBuckets(editAccount.account.id);
        form.setFieldsValue({
          accountName: editAccount.account.name || '',
          accessKeyId: editAccount.account.access_key_id,
          secretAccessKey: editAccount.account.secret_access_key,
          endpointScheme: editAccount.account.endpoint_scheme,
          endpointHost: editAccount.account.endpoint_host,
          forcePathStyle: true,
        });
      }
    } else if (mode === 'edit-token' && editToken) {
      setProvider('r2');
      // Load existing buckets for this token
      loadExistingBuckets(editToken.id);
      const account = accounts.find(
        (a) => a.provider === 'r2' && a.account.id === editToken.account_id
      );
      form.setFieldsValue({
        accountId: editToken.account_id,
        accountName: account?.account.name || '',
        tokenName: editToken.name || '',
        apiToken: editToken.api_token,
        accessKeyId: editToken.access_key_id,
        secretAccessKey: editToken.secret_access_key,
      });
    } else if (mode === 'add-token' && parentAccountId) {
      setProvider('r2');
      const account = accounts.find(
        (a) => a.provider === 'r2' && a.account.id === parentAccountId
      );
      form.setFieldsValue({
        accountId: parentAccountId,
        accountName: account?.account.name || '',
      });
    }
  }, [open, mode, editAccount, editToken, parentAccountId, form, accounts]);

  async function loadExistingBuckets(tokenId: number) {
    try {
      const existingBuckets = await invoke<Bucket[]>('list_buckets', { tokenId });
      const bucketConfigs = existingBuckets.map((b) => ({
        name: b.name,
        publicDomainHost: b.public_domain || undefined,
        publicDomainScheme: b.public_domain_scheme || undefined,
      }));
      setBuckets(bucketConfigs);
      if (bucketConfigs.length > 0) {
        form.setFieldValue('selectedBucket', bucketConfigs[0].name);
      }
    } catch (e) {
      console.error('Failed to load existing buckets:', e);
    }
  }

  async function loadAwsBuckets(accountId: string) {
    try {
      const existingBuckets = await invoke<AwsBucket[]>('list_aws_bucket_configs', { accountId });
      const bucketConfigs = existingBuckets.map((b) => ({
        name: b.name,
        publicDomainHost: b.public_domain_host || undefined,
        publicDomainScheme: b.public_domain_scheme || undefined,
      }));
      setBuckets(bucketConfigs);
      if (bucketConfigs.length > 0) {
        form.setFieldValue('selectedBucket', bucketConfigs[0].name);
      }
    } catch (e) {
      console.error('Failed to load AWS bucket configs:', e);
    }
  }

  async function loadMinioBuckets(accountId: string) {
    try {
      const existingBuckets = await invoke<MinioBucket[]>('list_minio_bucket_configs', { accountId });
      const bucketConfigs = existingBuckets.map((b) => ({
        name: b.name,
      }));
      setBuckets(bucketConfigs);
      if (bucketConfigs.length > 0) {
        form.setFieldValue('selectedBucket', bucketConfigs[0].name);
      }
    } catch (e) {
      console.error('Failed to load MinIO bucket configs:', e);
    }
  }

  async function loadRustfsBuckets(accountId: string) {
    try {
      const existingBuckets = await invoke<RustfsBucket[]>('list_rustfs_bucket_configs', { accountId });
      const bucketConfigs = existingBuckets.map((b) => ({
        name: b.name,
      }));
      setBuckets(bucketConfigs);
      if (bucketConfigs.length > 0) {
        form.setFieldValue('selectedBucket', bucketConfigs[0].name);
      }
    } catch (e) {
      console.error('Failed to load RustFS bucket configs:', e);
    }
  }

  function handleAddBucket() {
    const name = newBucketName.trim();
    if (name && !buckets.some((b) => b.name === name)) {
      setBuckets([
        ...buckets,
        showDomainSettings ? { name, publicDomainScheme: 'https' } : { name },
      ]);
      if (!selectedBucket) {
        form.setFieldValue('selectedBucket', name);
      }
    }
    setNewBucketName('');
    setAddingBucket(false);
  }

  function handleRemoveBucket(bucketName: string) {
    const newBuckets = buckets.filter((b) => b.name !== bucketName);
    setBuckets(newBuckets);
    if (selectedBucket === bucketName) {
      form.setFieldValue('selectedBucket', newBuckets[0]?.name || '');
    }
  }

  function parseDomainInput(value: string): { scheme?: string; host: string } {
    const trimmed = value.trim();
    if (!trimmed) return { host: '' };
    if (trimmed.includes('://')) {
      try {
        const url = new URL(trimmed);
        return {
          scheme: url.protocol.replace(':', ''),
          host: url.host,
        };
      } catch {
        return { host: trimmed.replace(/\/+$/, '') };
      }
    }
    return { host: trimmed.replace(/\/+$/, '') };
  }

  function handleDomainChange(bucketName: string, domain: string) {
    const parsed = parseDomainInput(domain);
    setBuckets(
      buckets.map((b) =>
        b.name === bucketName
          ? {
              ...b,
              publicDomainHost: parsed.host || undefined,
              publicDomainScheme: parsed.scheme || b.publicDomainScheme || 'https',
            }
          : b
      )
    );
  }

  function handleDomainSchemeChange(bucketName: string, scheme: string) {
    setBuckets(
      buckets.map((b) =>
        b.name === bucketName ? { ...b, publicDomainScheme: scheme } : b
      )
    );
  }

  async function handleLoadBuckets() {
    const accountId = form.getFieldValue('accountId');
    const accessKeyId = form.getFieldValue('accessKeyId');
    const secretAccessKey = form.getFieldValue('secretAccessKey');
    const region = form.getFieldValue('region');
    const endpointScheme = form.getFieldValue('endpointScheme') || 'https';
    const endpointHost = form.getFieldValue('endpointHost');
    const forcePathStyle = provider === 'rustfs' ? true : form.getFieldValue('forcePathStyle') || false;

    if (provider === 'r2' && (!accountId || !accessKeyId || !secretAccessKey)) {
      message.warning('Please enter Account ID, Access Key ID, and Secret Access Key first');
      return;
    }

    if (provider === 'aws' && (!accessKeyId || !secretAccessKey || !region)) {
      message.warning('Please enter Region, Access Key ID, and Secret Access Key first');
      return;
    }

    if (
      (provider === 'minio' || provider === 'rustfs') &&
      (!accessKeyId || !secretAccessKey || !endpointHost)
    ) {
      message.warning('Please enter Endpoint, Access Key ID, and Secret Access Key first');
      return;
    }

    setLoadingBuckets(true);
    try {
      const result = await listBuckets(
        provider === 'r2'
          ? {
              provider: 'r2',
              accountId,
              bucket: '',
              accessKeyId,
              secretAccessKey,
            }
          : provider === 'aws'
            ? {
                provider: 'aws',
                accountId: editAccount?.provider === 'aws' ? editAccount.account.id : 'aws',
                bucket: '',
                accessKeyId,
                secretAccessKey,
                region,
                endpointScheme: endpointScheme || undefined,
                endpointHost: endpointHost || undefined,
                forcePathStyle,
              }
            : provider === 'minio'
              ? {
                  provider: 'minio',
                  accountId: editAccount?.provider === 'minio' ? editAccount.account.id : 'minio',
                  bucket: '',
                  accessKeyId,
                  secretAccessKey,
                  endpointScheme,
                  endpointHost,
                  forcePathStyle,
                }
              : {
                  provider: 'rustfs',
                  accountId: editAccount?.provider === 'rustfs' ? editAccount.account.id : 'rustfs',
                  bucket: '',
                  accessKeyId,
                  secretAccessKey,
                  endpointScheme,
                  endpointHost,
                  forcePathStyle: true,
                }
      );
      // Merge with existing buckets to preserve domain settings
    const newBuckets = result.map((b) => {
      const existing = buckets.find((eb) => eb.name === b.name);
      if (existing) {
        return showDomainSettings
          ? existing
          : {
              name: existing.name,
            };
      }
      return showDomainSettings ? { name: b.name, publicDomainScheme: 'https' } : { name: b.name };
    });
      setBuckets(newBuckets);
      message.success(`Found ${newBuckets.length} bucket(s)`);

      // Auto-select first bucket if none selected
      if (newBuckets.length > 0 && !form.getFieldValue('selectedBucket')) {
        form.setFieldValue('selectedBucket', newBuckets[0].name);
      }
    } catch (e) {
      console.error('Failed to load buckets:', e);
      message.error(e instanceof Error ? e.message : 'Failed to load buckets');
    } finally {
      setLoadingBuckets(false);
    }
  }

  async function handleSubmit(values: FormValues) {
    setSaving(true);
    try {
      if (mode === 'edit-account') {
        if (provider === 'r2') {
          await updateAccountFn(values.accountId, values.accountName);
          message.success('Account updated');
        } else if (provider === 'aws' && editAccount?.provider === 'aws') {
          await updateAwsAccount({
            id: editAccount.account.id,
            name: values.accountName,
            access_key_id: values.accessKeyId,
            secret_access_key: values.secretAccessKey,
            region: values.region || '',
            endpoint_scheme: values.endpointScheme || null,
            endpoint_host: values.endpointHost || null,
            force_path_style: values.forcePathStyle ?? false,
          });
          await saveAwsBuckets(
            editAccount.account.id,
            buckets.map((b) => ({
              name: b.name,
              public_domain_scheme: b.publicDomainScheme || null,
              public_domain_host: b.publicDomainHost || null,
            }))
          );
          message.success('Account updated');
        } else if (provider === 'minio' && editAccount?.provider === 'minio') {
          await updateMinioAccount({
            id: editAccount.account.id,
            name: values.accountName,
            access_key_id: values.accessKeyId,
            secret_access_key: values.secretAccessKey,
            endpoint_scheme: values.endpointScheme || 'https',
            endpoint_host: values.endpointHost || '',
            force_path_style: values.forcePathStyle ?? false,
          });
          await saveMinioBuckets(
            editAccount.account.id,
            buckets.map((b) => ({
              name: b.name,
            }))
          );
          message.success('Account updated');
        } else if (provider === 'rustfs' && editAccount?.provider === 'rustfs') {
          await updateRustfsAccount({
            id: editAccount.account.id,
            name: values.accountName,
            access_key_id: values.accessKeyId,
            secret_access_key: values.secretAccessKey,
            endpoint_scheme: values.endpointScheme || 'https',
            endpoint_host: values.endpointHost || '',
          });
          await saveRustfsBuckets(
            editAccount.account.id,
            buckets.map((b) => ({
              name: b.name,
            }))
          );
          message.success('Account updated');
        }
      } else if (mode === 'add-account') {
        if (buckets.length === 0) {
          message.error('Please add at least one bucket');
          setSaving(false);
          return;
        }

        if (provider === 'r2') {
          await createAccount(values.accountId, values.accountName);
          const token = await createToken({
            account_id: values.accountId,
            name: values.tokenName,
            api_token: values.apiToken,
            access_key_id: values.accessKeyId,
            secret_access_key: values.secretAccessKey,
          });

          await saveBuckets(
            token.id,
            buckets.map((b) => ({
              name: b.name,
              public_domain: b.publicDomainHost || null,
              public_domain_scheme: b.publicDomainScheme || null,
            }))
          );

          await selectR2Bucket(token.id, values.selectedBucket || buckets[0].name);
          message.success('Account created and configured');
        } else if (provider === 'aws') {
          const account = await createAwsAccount({
            name: values.accountName,
            access_key_id: values.accessKeyId,
            secret_access_key: values.secretAccessKey,
            region: values.region || '',
            endpoint_scheme: values.endpointScheme || null,
            endpoint_host: values.endpointHost || null,
            force_path_style: values.forcePathStyle ?? false,
          });

          await saveAwsBuckets(
            account.id,
            buckets.map((b) => ({
              name: b.name,
              public_domain_scheme: b.publicDomainScheme || null,
              public_domain_host: b.publicDomainHost || null,
            }))
          );

          await selectAwsBucket(account.id, values.selectedBucket || buckets[0].name);
          message.success('Account created and configured');
        } else if (provider === 'minio') {
          const account = await createMinioAccount({
            name: values.accountName,
            access_key_id: values.accessKeyId,
            secret_access_key: values.secretAccessKey,
            endpoint_scheme: values.endpointScheme || 'https',
            endpoint_host: values.endpointHost || '',
            force_path_style: values.forcePathStyle ?? false,
          });

          await saveMinioBuckets(
            account.id,
            buckets.map((b) => ({
              name: b.name,
            }))
          );

          await selectMinioBucket(account.id, values.selectedBucket || buckets[0].name);
          message.success('Account created and configured');
        } else if (provider === 'rustfs') {
          const account = await createRustfsAccount({
            name: values.accountName,
            access_key_id: values.accessKeyId,
            secret_access_key: values.secretAccessKey,
            endpoint_scheme: values.endpointScheme || 'https',
            endpoint_host: values.endpointHost || '',
          });

          await saveRustfsBuckets(
            account.id,
            buckets.map((b) => ({
              name: b.name,
            }))
          );

          await selectRustfsBucket(account.id, values.selectedBucket || buckets[0].name);
          message.success('Account created and configured');
        }
      } else if (mode === 'add-token') {
        if (buckets.length === 0) {
          message.error('Please add at least one bucket');
          setSaving(false);
          return;
        }

        await updateAccountFn(values.accountId, values.accountName);

        const token = await createToken({
          account_id: values.accountId,
          name: values.tokenName,
          api_token: values.apiToken,
          access_key_id: values.accessKeyId,
          secret_access_key: values.secretAccessKey,
        });

        await saveBuckets(
          token.id,
          buckets.map((b) => ({
            name: b.name,
            public_domain: b.publicDomainHost || null,
            public_domain_scheme: b.publicDomainScheme || null,
          }))
        );

        message.success('Token added');
      } else if (mode === 'edit-token' && editToken) {
        await updateAccountFn(values.accountId, values.accountName);

        await updateTokenFn({
          id: editToken.id,
          name: values.tokenName,
          api_token: values.apiToken,
          access_key_id: values.accessKeyId,
          secret_access_key: values.secretAccessKey,
        });

        if (buckets.length > 0) {
          await saveBuckets(
            editToken.id,
            buckets.map((b) => ({
              name: b.name,
              public_domain: b.publicDomainHost || null,
              public_domain_scheme: b.publicDomainScheme || null,
            }))
          );
        }

        message.success('Token updated');
      }

      onClose();
    } catch (e) {
      console.error('Save failed:', e);
      message.error(e instanceof Error ? e.message : 'Failed to save');
    } finally {
      setSaving(false);
    }
  }

  function getTitle() {
    const providerLabel =
      provider === 'r2'
        ? 'R2'
        : provider === 'aws'
          ? 'AWS'
          : provider === 'minio'
            ? 'MinIO'
            : 'RustFS';
    switch (mode) {
      case 'add-account':
        return `Add ${providerLabel} Account`;
      case 'edit-account':
        return `Edit ${providerLabel} Account`;
      case 'add-token':
        return 'Add Token';
      case 'edit-token':
        return 'Edit Token';
    }
  }

  function getIcon() {
    return isAccountMode ? (
      <UserOutlined style={{ fontSize: 40, color: '#f6821f' }} />
    ) : (
      <KeyOutlined style={{ fontSize: 40, color: '#f6821f' }} />
    );
  }

  return (
    <Modal open={open} onCancel={onClose} footer={null} width={480} centered destroyOnHidden>
      <div style={{ textAlign: 'center', marginBottom: 16 }}>
        {getIcon()}
        <h3 style={{ marginTop: 8, marginBottom: 0 }}>{getTitle()}</h3>
        {mode === 'add-account' && (
          <Space style={{ justifyContent: 'center', marginTop: 8 }}>
            {(['r2', 'aws', 'minio', 'rustfs'] as StorageProvider[]).map((item) => (
              <Tag.CheckableTag
                key={item}
                checked={provider === item}
                onChange={() => setProvider(item)}
              >
                {item === 'r2'
                  ? 'R2'
                  : item === 'aws'
                    ? 'AWS'
                    : item === 'minio'
                      ? 'MinIO'
                      : 'RustFS'}
              </Tag.CheckableTag>
            ))}
          </Space>
        )}
      </div>

      <Form
        form={form}
        layout="vertical"
        onFinish={handleSubmit}
        autoComplete="off"
        size="small"
        style={{ '--ant-form-item-margin-bottom': '12px' } as React.CSSProperties}
      >
        {isR2Provider && (
          <Form.Item
            label="Account ID"
            name="accountId"
            rules={[{ required: true, message: 'Required' }]}
            style={{ marginBottom: 12 }}
          >
            <Input
              placeholder="Cloudflare Account ID"
              disabled={isEditMode || mode === 'add-token'}
              autoComplete="off"
              autoCorrect="off"
              autoCapitalize="off"
              prefix={<UserOutlined />}
            />
          </Form.Item>
        )}

        {/* Account Name - always show */}
        <Form.Item label="Account Name" name="accountName" style={{ marginBottom: 12 }}>
          <Input
            placeholder="My Account (optional)"
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="off"
          />
        </Form.Item>

        {/* R2 token/credentials */}
        {isR2Provider && mode !== 'edit-account' && (
          <>
            <Divider plain style={{ margin: '12px 0 8px' }}>
              <span style={{ fontSize: 12 }}>
                <KeyOutlined /> Token Credentials
              </span>
            </Divider>

            <Form.Item label="Token Name" name="tokenName" style={{ marginBottom: 12 }}>
              <Input
                placeholder="Production / Staging (optional)"
                autoComplete="off"
                autoCorrect="off"
                autoCapitalize="off"
              />
            </Form.Item>

            <Form.Item
              label="API Token"
              name="apiToken"
              rules={[{ required: true, message: 'Required' }]}
              style={{ marginBottom: 12 }}
            >
              <Input.Password placeholder="Cloudflare API Token" />
            </Form.Item>

            <Form.Item
              label="Access Key ID"
              name="accessKeyId"
              rules={[{ required: true, message: 'Required' }]}
              style={{ marginBottom: 12 }}
            >
              <Input
                placeholder="S3-compatible Access Key ID"
                autoComplete="off"
                autoCorrect="off"
                autoCapitalize="off"
              />
            </Form.Item>

            <Form.Item
              label="Secret Access Key"
              name="secretAccessKey"
              rules={[{ required: true, message: 'Required' }]}
              style={{ marginBottom: 12 }}
            >
              <Input.Password placeholder="Secret Access Key" />
            </Form.Item>
          </>
        )}

        {/* AWS / MinIO credentials */}
        {!isR2Provider && isAccountMode && (
          <>
            <Divider plain style={{ margin: '12px 0 8px' }}>
              <span style={{ fontSize: 12 }}>
                <KeyOutlined /> Access Credentials
              </span>
            </Divider>

            <Form.Item
              label="Access Key ID"
              name="accessKeyId"
              rules={[{ required: true, message: 'Required' }]}
              style={{ marginBottom: 12 }}
            >
              <Input
                placeholder="Access Key ID"
                autoComplete="off"
                autoCorrect="off"
                autoCapitalize="off"
              />
            </Form.Item>

            <Form.Item
              label="Secret Access Key"
              name="secretAccessKey"
              rules={[{ required: true, message: 'Required' }]}
              style={{ marginBottom: 12 }}
            >
              <Input.Password placeholder="Secret Access Key" />
            </Form.Item>

            {provider === 'aws' && (
              <Form.Item
                label="Region"
                name="region"
                rules={[{ required: true, message: 'Required' }]}
                style={{ marginBottom: 12 }}
              >
                <Input placeholder="us-east-1" autoComplete="off" autoCorrect="off" />
              </Form.Item>
            )}

            <Form.Item label="Endpoint" style={{ marginBottom: 12 }}>
              <Space.Compact size="small" style={{ width: '100%' }}>
                <Form.Item name="endpointScheme" noStyle initialValue="https">
                  <Select style={{ width: 90 }} options={[{ value: 'https' }, { value: 'http' }]} />
                </Form.Item>
                <Form.Item
                  name="endpointHost"
                  noStyle
                  rules={[{ required: provider === 'minio' || provider === 'rustfs', message: 'Required' }]}
                >
                  <Input
                    placeholder={
                      provider === 'aws'
                        ? 'custom endpoint (optional)'
                        : provider === 'rustfs'
                          ? 'rustfs.example.com:9000'
                          : 'minio.example.com:9000'
                    }
                    autoComplete="off"
                    autoCorrect="off"
                    autoCapitalize="off"
                    onBlur={(e) => {
                      const parsed = parseDomainInput(e.target.value);
                      form.setFieldsValue({
                        endpointHost: parsed.host,
                        endpointScheme: parsed.scheme || form.getFieldValue('endpointScheme'),
                      });
                    }}
                  />
                </Form.Item>
              </Space.Compact>
            </Form.Item>

            {provider !== 'rustfs' && (
              <Form.Item label="Force Path Style" name="forcePathStyle" valuePropName="checked">
                <Switch />
              </Form.Item>
            )}
          </>
        )}

        {/* Buckets */}
        {(isR2Provider ? mode !== 'edit-account' : isAccountMode) && (
          <>
            <Divider plain style={{ margin: '12px 0 8px' }}>
              <span style={{ fontSize: 12 }}>Buckets</span>
            </Divider>

            <div
              style={{
                display: 'flex',
                justifyContent: 'flex-end',
                gap: 8,
                marginBottom: 6,
              }}
            >
              <Button
                type="link"
                size="small"
                icon={<PlusOutlined />}
                onClick={() => setAddingBucket(true)}
                style={{ padding: 0, height: 'auto', fontSize: 12 }}
              >
                Add
              </Button>
              <Button
                type="link"
                size="small"
                icon={<ReloadOutlined spin={loadingBuckets} />}
                onClick={handleLoadBuckets}
                loading={loadingBuckets}
                style={{ padding: 0, height: 'auto', fontSize: 12 }}
              >
                Load
              </Button>
            </div>

            <Form.Item name="selectedBucket" hidden>
              <Input type="hidden" />
            </Form.Item>

            <div style={{ marginBottom: 12, maxHeight: 160, overflowY: 'auto' }}>
              {buckets.map((bucket) => {
                const isSelected = selectedBucket === bucket.name;
                return (
                  <div
                    key={bucket.name}
                    style={{
                      display: 'flex',
                      alignItems: 'center',
                      gap: 6,
                      padding: '4px 8px',
                      marginBottom: 2,
                      borderRadius: 4,
                      border: isSelected
                        ? '1px solid var(--ant-color-primary)'
                        : '1px solid var(--ant-color-border)',
                      background: isSelected ? 'var(--ant-color-primary-bg)' : undefined,
                      cursor: 'pointer',
                    }}
                    onClick={() => form.setFieldValue('selectedBucket', bucket.name)}
                  >
                    {isSelected && (
                      <CheckOutlined
                        style={{ color: 'var(--ant-color-primary)', flexShrink: 0, fontSize: 12 }}
                      />
                    )}
                    <span
                      style={{
                        fontWeight: isSelected ? 500 : 400,
                        fontSize: 12,
                        minWidth: 50,
                        maxWidth: 100,
                        flexShrink: 0,
                        overflow: 'hidden',
                        textOverflow: 'ellipsis',
                        whiteSpace: 'nowrap',
                      }}
                      title={bucket.name}
                    >
                      {bucket.name}
                    </span>
                    {showDomainSettings ? (
                      <Space.Compact size="small" style={{ flex: 1 }}>
                        <Select
                          size="small"
                          value={bucket.publicDomainScheme || 'https'}
                          onChange={(value) => handleDomainSchemeChange(bucket.name, value)}
                          onClick={(e) => e.stopPropagation()}
                          style={{ width: 90 }}
                          options={[{ value: 'https' }, { value: 'http' }]}
                        />
                        <Input
                          placeholder="domain.com"
                          value={bucket.publicDomainHost || ''}
                          onChange={(e) => handleDomainChange(bucket.name, e.target.value)}
                          onClick={(e) => e.stopPropagation()}
                          style={{ flex: 1, fontSize: 12 }}
                          autoComplete="off"
                          autoCorrect="off"
                          autoCapitalize="off"
                        />
                      </Space.Compact>
                    ) : (
                      <div style={{ flex: 1 }} />
                    )}
                    <Button
                      type="text"
                      size="small"
                      danger
                      icon={<DeleteOutlined style={{ fontSize: 12 }} />}
                      onClick={(e) => {
                        e.stopPropagation();
                        handleRemoveBucket(bucket.name);
                      }}
                      style={{ padding: '0 4px', height: 'auto' }}
                    />
                  </div>
                );
              })}

              {addingBucket && (
                <div style={{ display: 'flex', gap: 6, padding: '4px 0' }}>
                  <Input
                    size="small"
                    value={newBucketName}
                    onChange={(e) => setNewBucketName(e.target.value)}
                    onBlur={() => {
                      if (newBucketName.trim()) handleAddBucket();
                      else setAddingBucket(false);
                    }}
                    onPressEnter={handleAddBucket}
                    autoFocus
                    placeholder="Enter bucket name"
                    style={{ flex: 1, fontSize: 12 }}
                  />
                </div>
              )}

              {buckets.length === 0 && !addingBucket && (
                <div style={{ padding: '8px 0', textAlign: 'center', color: '#999', fontSize: 12 }}>
                  Click &quot;Load&quot; to fetch or &quot;Add&quot; manually
                </div>
              )}
            </div>
          </>
        )}

        <Form.Item style={{ marginBottom: 0, marginTop: 12 }}>
          <Button type="primary" htmlType="submit" loading={saving} block>
            {isEditMode ? 'Save Changes' : 'Add'}
          </Button>
        </Form.Item>
      </Form>
    </Modal>
  );
}
