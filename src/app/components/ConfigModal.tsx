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
import { listR2Buckets } from '../lib/r2cache';
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
  const accounts = useAccountStore((state) => state.accounts);
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
      // Get account name from store
      const account = accounts.find((a) => a.account.id === editToken.account_id);
      form.setFieldsValue({
        accountId: editToken.account_id,
        accountName: account?.account.name || '',
        tokenName: editToken.name || '',
        apiToken: editToken.api_token,
        accessKeyId: editToken.access_key_id,
        secretAccessKey: editToken.secret_access_key,
      });
    } else if (mode === 'add-token' && parentAccountId) {
      // Get account name from store
      const account = accounts.find((a) => a.account.id === parentAccountId);
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
    const accessKeyId = form.getFieldValue('accessKeyId');
    const secretAccessKey = form.getFieldValue('secretAccessKey');

    if (!accountId || !accessKeyId || !secretAccessKey) {
      message.warning('Please enter Account ID, Access Key ID, and Secret Access Key first');
      return;
    }

    setLoadingBuckets(true);
    try {
      const result = await listR2Buckets(accountId, accessKeyId, secretAccessKey);
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

        // Update account name if changed
        await updateAccountFn(values.accountId, values.accountName);

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
        // Update account name if changed
        await updateAccountFn(values.accountId, values.accountName);

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
    <Modal open={open} onCancel={onClose} footer={null} width={480} centered destroyOnHidden>
      <div style={{ textAlign: 'center', marginBottom: 16 }}>
        {getIcon()}
        <h3 style={{ marginTop: 8, marginBottom: 0 }}>{getTitle()}</h3>
      </div>

      <Form
        form={form}
        layout="vertical"
        onFinish={handleSubmit}
        autoComplete="off"
        size="small"
        style={{ '--ant-form-item-margin-bottom': '12px' } as React.CSSProperties}
      >
        {/* Account ID - only editable when adding */}
        <Form.Item
          label="Account ID"
          name="accountId"
          rules={[{ required: true, message: 'Required' }]}
          style={{ marginBottom: 12 }}
        >
          <Input
            placeholder="Cloudflare Account ID"
            disabled={isEditMode || mode === 'add-token'}
            prefix={<UserOutlined />}
          />
        </Form.Item>

        {/* Account Name - always show */}
        <Form.Item label="Account Name" name="accountName" style={{ marginBottom: 12 }}>
          <Input placeholder="My Account (optional)" />
        </Form.Item>

        {/* Token fields - not shown for edit-account mode */}
        {mode !== 'edit-account' && (
          <>
            <Divider plain style={{ margin: '12px 0 8px' }}>
              <span style={{ fontSize: 12 }}>
                <KeyOutlined /> Token Credentials
              </span>
            </Divider>

            <Form.Item label="Token Name" name="tokenName" style={{ marginBottom: 12 }}>
              <Input placeholder="Production / Staging (optional)" />
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
              <Input placeholder="S3-compatible Access Key ID" />
            </Form.Item>

            <Form.Item
              label="Secret Access Key"
              name="secretAccessKey"
              rules={[{ required: true, message: 'Required' }]}
              style={{ marginBottom: 12 }}
            >
              <Input.Password placeholder="Secret Access Key" />
            </Form.Item>

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
                    <Space.Compact size="small" style={{ flex: 1 }}>
                      <Input
                        style={{ width: 60, flexShrink: 0, fontSize: 12 }}
                        value="https://"
                        disabled
                      />
                      <Input
                        placeholder="domain.com"
                        value={bucket.publicDomain || ''}
                        onChange={(e) => handleDomainChange(bucket.name, e.target.value)}
                        onClick={(e) => e.stopPropagation()}
                        style={{ flex: 1, fontSize: 12 }}
                      />
                    </Space.Compact>
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
