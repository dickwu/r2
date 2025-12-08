'use client';

import { useState, useEffect } from 'react';
import { Store } from '@tauri-apps/plugin-store';
import { Form, Input, Button, Modal, App, Space } from 'antd';
import {
  CloudOutlined,
  ReloadOutlined,
  PlusOutlined,
  DeleteOutlined,
  CheckOutlined,
  KeyOutlined,
} from '@ant-design/icons';
import { listR2Objects, listR2Buckets } from '../lib/r2api';
import AccessKeyModal from './AccessKeyModal';

export interface BucketConfig {
  name: string;
  publicDomain?: string;
}

export interface R2Config {
  accountId: string;
  token: string;
  accessKeyId?: string; // S3-compatible Access Key ID
  secretAccessKey?: string; // S3-compatible Secret Access Key
  bucket: string; // Currently selected bucket
  buckets?: BucketConfig[]; // All buckets with their domains
  publicDomain?: string; // Derived from selected bucket for convenience
}

interface ConfigModalProps {
  open: boolean;
  onClose: () => void;
  onSave: (config: R2Config) => void;
  initialConfig?: R2Config | null;
}

export default function ConfigModal({ open, onClose, onSave, initialConfig }: ConfigModalProps) {
  const [saving, setSaving] = useState(false);
  const [loadingBuckets, setLoadingBuckets] = useState(false);
  const [buckets, setBuckets] = useState<BucketConfig[]>([]);
  const [addingBucket, setAddingBucket] = useState(false);
  const [newBucketName, setNewBucketName] = useState('');
  const [accessKeyModalOpen, setAccessKeyModalOpen] = useState(false);
  const [form] = Form.useForm<R2Config>();
  const selectedBucket = Form.useWatch('bucket', form);
  const { message } = App.useApp();

  useEffect(() => {
    if (open && initialConfig) {
      form.setFieldsValue(initialConfig);
      if (initialConfig.buckets?.length) {
        setBuckets(initialConfig.buckets);
      }
    }
  }, [open, initialConfig, form]);

  function handleAddBucket() {
    const name = newBucketName.trim();
    if (name && !buckets.some((b) => b.name === name)) {
      setBuckets([...buckets, { name }]);
      form.setFieldValue('bucket', name);
    }
    setNewBucketName('');
    setAddingBucket(false);
  }

  function handleRemoveBucket(bucketName: string) {
    const newBuckets = buckets.filter((b) => b.name !== bucketName);
    setBuckets(newBuckets);
    if (selectedBucket === bucketName) {
      form.setFieldValue('bucket', newBuckets[0]?.name || '');
    }
  }

  function handleDomainChange(bucketName: string, domain: string) {
    setBuckets(
      buckets.map((b) => (b.name === bucketName ? { ...b, publicDomain: domain || undefined } : b))
    );
  }

  async function handleLoadBuckets() {
    const accountId = form.getFieldValue('accountId');
    const token = form.getFieldValue('token');

    if (!accountId || !token) {
      message.warning('Please enter Account ID and API Token first');
      return;
    }

    setLoadingBuckets(true);
    try {
      const result = await listR2Buckets(accountId, token);
      // Merge with existing buckets to preserve domain settings
      const newBuckets = result.map((b) => {
        const existing = buckets.find((eb) => eb.name === b.name);
        return existing || { name: b.name };
      });
      setBuckets(newBuckets);
      message.success(`Found ${newBuckets.length} bucket(s)`);

      // Auto-select first bucket if none selected
      if (newBuckets.length > 0 && !form.getFieldValue('bucket')) {
        form.setFieldValue('bucket', newBuckets[0].name);
      }
    } catch (e) {
      console.error('Failed to load buckets:', e);
      message.error(e instanceof Error ? e.message : 'Failed to load buckets');
    } finally {
      setLoadingBuckets(false);
    }
  }

  async function handleSubmit(values: R2Config) {
    setSaving(true);
    try {
      // Validate credentials by testing API connection
      await listR2Objects(values, { perPage: 1 });

      // Get publicDomain for the selected bucket
      const selectedBucketConfig = buckets.find((b) => b.name === values.bucket);

      const configToSave: R2Config = {
        ...values,
        buckets: buckets.length > 0 ? buckets : undefined,
        publicDomain: selectedBucketConfig?.publicDomain,
      };

      // API works, save config
      const store = await Store.load('r2-config.json');
      await store.set('config', configToSave);
      await store.save();
      message.success('Connection verified and configuration saved');
      onSave(configToSave);
      onClose();
    } catch (e) {
      console.error('API validation failed:', e);
      message.error(e instanceof Error ? e.message : 'Failed to connect to R2');
    } finally {
      setSaving(false);
    }
  }

  return (
    <Modal open={open} onCancel={onClose} footer={null} width={500} centered destroyOnHidden>
      <div style={{ textAlign: 'center', marginBottom: 24 }}>
        <CloudOutlined style={{ fontSize: 40, color: '#f6821f' }} />
        <h3 style={{ marginTop: 12, marginBottom: 4 }}>Cloudflare R2</h3>
      </div>

      <Form form={form} layout="vertical" onFinish={handleSubmit} autoComplete="off">
        <Form.Item
          label="Account ID"
          name="accountId"
          rules={[{ required: true, message: 'Required' }]}
        >
          <Input placeholder="Cloudflare Account ID" />
        </Form.Item>

        <Form.Item
          label="API Token"
          name="token"
          rules={[{ required: true, message: 'Required' }]}
          extra="R2 read/write token for API access"
        >
          <Input.Password placeholder="API Token" />
        </Form.Item>

        <div
          style={{
            display: 'flex',
            justifyContent: 'space-between',
            alignItems: 'center',
            marginBottom: 8,
          }}
        >
          <span>
            <span style={{ color: '#ff4d4f', marginRight: 4 }}>*</span>
            Buckets
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
        <Form.Item
          name="bucket"
          rules={[{ required: true, message: 'Please select a bucket' }]}
          style={{ display: 'none' }}
        >
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
                onClick={() => form.setFieldValue('bucket', bucket.name)}
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

        <Form.Item style={{ marginBottom: 0 }}>
          <Space orientation="vertical" style={{ width: '100%' }}>
            <Button type="primary" htmlType="submit" loading={saving} block>
              Save
            </Button>
            <Button
              type="default"
              icon={<KeyOutlined />}
              onClick={() => setAccessKeyModalOpen(true)}
              block
            >
              S3 Access Config
            </Button>
          </Space>
        </Form.Item>
      </Form>

      <AccessKeyModal
        open={accessKeyModalOpen}
        onClose={() => setAccessKeyModalOpen(false)}
        onSave={(accessKeyId, secretAccessKey) => {
          form.setFieldsValue({ accessKeyId, secretAccessKey });
        }}
        initialAccessKeyId={form.getFieldValue('accessKeyId')}
        initialSecretAccessKey={form.getFieldValue('secretAccessKey')}
      />
    </Modal>
  );
}
