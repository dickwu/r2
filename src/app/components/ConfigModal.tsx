'use client';

import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Form, Input, Button, Modal, App, Space, Divider } from 'antd';
import {
  ReloadOutlined,
  PlusOutlined,
  DeleteOutlined,
  CheckOutlined,
  UserOutlined,
  KeyOutlined,
} from '@ant-design/icons';
import { listR2Buckets } from '../lib/r2api';
import { useAccountStore, Account, Token, Bucket } from '../stores/accountStore';

export interface BucketConfig {
  name: string;
  publicDomain?: string;
}

// Legacy R2Config for compatibility during transition
export interface R2Config {
  accountId: string;
  token: string;
  accessKeyId?: string;
  secretAccessKey?: string;
  bucket: string;
  buckets?: BucketConfig[];
  publicDomain?: string;
}

interface FormValues {
  accountId: string;
  accountName?: string;
  tokenName?: string;
  apiToken: string;
  accessKeyId: string;
  secretAccessKey: string;
  selectedBucket?: string;
}

export type ModalMode = 'add-account' | 'edit-account' | 'add-token' | 'edit-token';

interface ConfigModalProps {
  open: boolean;
  onClose: () => void;
  mode: ModalMode;
  editAccount?: Account | null;
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
  const [form] = Form.useForm<FormValues>();
  const selectedBucket = Form.useWatch('selectedBucket', form);
  const { message } = App.useApp();

  // Use Zustand store
  const createAccount = useAccountStore((state) => state.createAccount);
  const updateAccountFn = useAccountStore((state) => state.updateAccount);
  const createToken = useAccountStore((state) => state.createToken);
  const updateTokenFn = useAccountStore((state) => state.updateToken);
  const saveBuckets = useAccountStore((state) => state.saveBuckets);
  const selectBucket = useAccountStore((state) => state.selectBucket);

  // Determine if we're in account mode or token mode
  const isAccountMode = mode === 'add-account' || mode === 'edit-account';
  const isEditMode = mode === 'edit-account' || mode === 'edit-token';

  useEffect(() => {
    if (!open) {
      form.resetFields();
      setBuckets([]);
      return;
    }

    if (mode === 'edit-account' && editAccount) {
      form.setFieldsValue({
        accountId: editAccount.id,
        accountName: editAccount.name || '',
      });
    } else if (mode === 'edit-token' && editToken) {
      // Load existing buckets for this token
      loadExistingBuckets(editToken.id);
      form.setFieldsValue({
        accountId: editToken.account_id,
        tokenName: editToken.name || '',
        apiToken: editToken.api_token,
        accessKeyId: editToken.access_key_id,
        secretAccessKey: editToken.secret_access_key,
      });
    } else if (mode === 'add-token' && parentAccountId) {
      form.setFieldsValue({
        accountId: parentAccountId,
      });
    }
  }, [open, mode, editAccount, editToken, parentAccountId, form]);

  async function loadExistingBuckets(tokenId: number) {
    try {
      const existingBuckets = await invoke<Bucket[]>('list_buckets', { tokenId });
      const bucketConfigs = existingBuckets.map((b) => ({
        name: b.name,
        publicDomain: b.public_domain || undefined,
      }));
      setBuckets(bucketConfigs);
      if (bucketConfigs.length > 0) {
        form.setFieldValue('selectedBucket', bucketConfigs[0].name);
      }
    } catch (e) {
      console.error('Failed to load existing buckets:', e);
    }
  }

  function handleAddBucket() {
    const name = newBucketName.trim();
    if (name && !buckets.some((b) => b.name === name)) {
      setBuckets([...buckets, { name }]);
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

  function handleDomainChange(bucketName: string, domain: string) {
    setBuckets(
      buckets.map((b) => (b.name === bucketName ? { ...b, publicDomain: domain || undefined } : b))
    );
  }

  async function handleLoadBuckets() {
    const accountId = form.getFieldValue('accountId');
    const apiToken = form.getFieldValue('apiToken');

    if (!accountId || !apiToken) {
      message.warning('Please enter Account ID and API Token first');
      return;
    }

    setLoadingBuckets(true);
    try {
      const result = await listR2Buckets(accountId, apiToken);
      // Merge with existing buckets to preserve domain settings
      const newBuckets = result.map((b) => {
        const existing = buckets.find((eb) => eb.name === b.name);
        return existing || { name: b.name };
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
        // Update account name only
        await updateAccountFn(values.accountId, values.accountName);
        message.success('Account updated');
      } else if (mode === 'add-account') {
        // Create new account with initial token
        if (buckets.length === 0) {
          message.error('Please add at least one bucket');
          setSaving(false);
          return;
        }

        // Create account
        await createAccount(values.accountId, values.accountName);

        // Create token
        const token = await createToken({
          account_id: values.accountId,
          name: values.tokenName,
          api_token: values.apiToken,
          access_key_id: values.accessKeyId,
          secret_access_key: values.secretAccessKey,
        });

        // Save buckets
        await saveBuckets(
          token.id,
          buckets.map((b) => ({
            name: b.name,
            public_domain: b.publicDomain || null,
          }))
        );

        // Set as current selection
        await selectBucket(token.id, values.selectedBucket || buckets[0].name);

        message.success('Account created and configured');
      } else if (mode === 'add-token') {
        // Add new token to existing account
        if (buckets.length === 0) {
          message.error('Please add at least one bucket');
          setSaving(false);
          return;
        }

        const token = await createToken({
          account_id: values.accountId,
          name: values.tokenName,
          api_token: values.apiToken,
          access_key_id: values.accessKeyId,
          secret_access_key: values.secretAccessKey,
        });

        // Save buckets
        await saveBuckets(
          token.id,
          buckets.map((b) => ({
            name: b.name,
            public_domain: b.publicDomain || null,
          }))
        );

        message.success('Token added');
      } else if (mode === 'edit-token' && editToken) {
        // Update existing token
        await updateTokenFn({
          id: editToken.id,
          name: values.tokenName,
          api_token: values.apiToken,
          access_key_id: values.accessKeyId,
          secret_access_key: values.secretAccessKey,
        });

        // Update buckets
        if (buckets.length > 0) {
          await saveBuckets(
            editToken.id,
            buckets.map((b) => ({
              name: b.name,
              public_domain: b.publicDomain || null,
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
    switch (mode) {
      case 'add-account':
        return 'Add Account';
      case 'edit-account':
        return 'Edit Account';
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
    <Modal open={open} onCancel={onClose} footer={null} width={500} centered destroyOnHidden>
      <div style={{ textAlign: 'center', marginBottom: 24 }}>
        {getIcon()}
        <h3 style={{ marginTop: 12, marginBottom: 4 }}>{getTitle()}</h3>
        {mode === 'add-account' && (
          <p style={{ color: '#666', margin: 0 }}>Add a Cloudflare account with R2 access</p>
        )}
      </div>

      <Form form={form} layout="vertical" onFinish={handleSubmit} autoComplete="off">
        {/* Account ID - only editable when adding */}
        <Form.Item
          label="Account ID"
          name="accountId"
          rules={[{ required: true, message: 'Required' }]}
        >
          <Input
            placeholder="Cloudflare Account ID"
            disabled={isEditMode || mode === 'add-token'}
            prefix={<UserOutlined />}
          />
        </Form.Item>

        {/* Account Name - for account modes */}
        {isAccountMode && (
          <Form.Item
            label="Display Name"
            name="accountName"
            extra="Optional friendly name for this account"
          >
            <Input placeholder="My Account" />
          </Form.Item>
        )}

        {/* Token fields - not shown for edit-account mode */}
        {mode !== 'edit-account' && (
          <>
            <Divider plain>
              <Space>
                <KeyOutlined /> Token Credentials
              </Space>
            </Divider>

            <Form.Item
              label="Token Name"
              name="tokenName"
              extra="Optional name to identify this token"
            >
              <Input placeholder="Production / Staging / etc." />
            </Form.Item>

            <Form.Item
              label="API Token"
              name="apiToken"
              rules={[{ required: true, message: 'Required' }]}
              extra="Cloudflare API token with R2 permissions"
            >
              <Input.Password placeholder="API Token" />
            </Form.Item>

            <Form.Item
              label="Access Key ID"
              name="accessKeyId"
              rules={[{ required: true, message: 'Required' }]}
              extra="S3-compatible Access Key ID"
            >
              <Input placeholder="Access Key ID" />
            </Form.Item>

            <Form.Item
              label="Secret Access Key"
              name="secretAccessKey"
              rules={[{ required: true, message: 'Required' }]}
            >
              <Input.Password placeholder="Secret Access Key" />
            </Form.Item>

            <Divider plain>Buckets</Divider>

            <div
              style={{
                display: 'flex',
                justifyContent: 'space-between',
                alignItems: 'center',
                marginBottom: 8,
              }}
            >
              <span style={{ color: '#666', fontSize: 12 }}>
                Buckets accessible with this token
              </span>
              <Space size="small">
                <Button
                  type="link"
                  size="small"
                  icon={<PlusOutlined />}
                  onClick={() => setAddingBucket(true)}
                  style={{ padding: 0, height: 'auto' }}
                >
                  Add
                </Button>
                <Button
                  type="link"
                  size="small"
                  icon={<ReloadOutlined spin={loadingBuckets} />}
                  onClick={handleLoadBuckets}
                  loading={loadingBuckets}
                  style={{ padding: 0, height: 'auto' }}
                >
                  Load
                </Button>
              </Space>
            </div>

            <Form.Item name="selectedBucket" hidden>
              <Input type="hidden" />
            </Form.Item>

            <div style={{ marginBottom: 16, maxHeight: 200, overflowY: 'auto' }}>
              {buckets.map((bucket) => {
                const isSelected = selectedBucket === bucket.name;
                return (
                  <div
                    key={bucket.name}
                    style={{
                      display: 'flex',
                      alignItems: 'center',
                      gap: 8,
                      padding: '8px 12px',
                      marginBottom: 4,
                      borderRadius: 6,
                      border: isSelected
                        ? '1px solid var(--ant-color-primary)'
                        : '1px solid var(--ant-color-border)',
                      background: isSelected ? 'var(--ant-color-primary-bg)' : undefined,
                      cursor: 'pointer',
                    }}
                    onClick={() => form.setFieldValue('selectedBucket', bucket.name)}
                  >
                    {isSelected && (
                      <CheckOutlined style={{ color: 'var(--ant-color-primary)', flexShrink: 0 }} />
                    )}
                    <span
                      style={{
                        fontWeight: isSelected ? 500 : 400,
                        minWidth: 60,
                        maxWidth: 120,
                        flexShrink: 0,
                        overflow: 'hidden',
                        textOverflow: 'ellipsis',
                        whiteSpace: 'nowrap',
                      }}
                      title={bucket.name}
                    >
                      {bucket.name}
                    </span>
                    <Input
                      size="small"
                      addonBefore="https://"
                      placeholder="domain.com"
                      value={bucket.publicDomain || ''}
                      onChange={(e) => handleDomainChange(bucket.name, e.target.value)}
                      onClick={(e) => e.stopPropagation()}
                      style={{ flex: 1 }}
                    />
                    <Button
                      type="text"
                      size="small"
                      danger
                      icon={<DeleteOutlined />}
                      onClick={(e) => {
                        e.stopPropagation();
                        handleRemoveBucket(bucket.name);
                      }}
                    />
                  </div>
                );
              })}

              {addingBucket && (
                <div style={{ display: 'flex', gap: 8, padding: '8px 0' }}>
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
                    style={{ flex: 1 }}
                  />
                </div>
              )}

              {buckets.length === 0 && !addingBucket && (
                <div style={{ padding: '16px 0', textAlign: 'center', color: '#999' }}>
                  Click &quot;Load&quot; to fetch buckets or &quot;Add&quot; to add manually
                </div>
              )}
            </div>
          </>
        )}

        <Form.Item style={{ marginBottom: 0, marginTop: 16 }}>
          <Button type="primary" htmlType="submit" loading={saving} block>
            {isEditMode ? 'Save Changes' : 'Add'}
          </Button>
        </Form.Item>
      </Form>
    </Modal>
  );
}
