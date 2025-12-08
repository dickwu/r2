'use client';

import { useState, useEffect } from 'react';
import { Store } from '@tauri-apps/plugin-store';
import { Form, Input, Button, Modal, App, Tooltip } from 'antd';
import { KeyOutlined, QuestionCircleOutlined } from '@ant-design/icons';
import { openUrl } from '@tauri-apps/plugin-opener';
import { R2Config } from './ConfigModal';

interface AccessKeyModalProps {
  open: boolean;
  onClose: () => void;
  onSave: (accessKeyId: string, secretAccessKey: string) => void;
  initialAccessKeyId?: string;
  initialSecretAccessKey?: string;
}

interface FormValues {
  accessKeyId: string;
  secretAccessKey: string;
}

export default function AccessKeyModal({
  open,
  onClose,
  onSave,
  initialAccessKeyId,
  initialSecretAccessKey,
}: AccessKeyModalProps) {
  const [saving, setSaving] = useState(false);
  const [form] = Form.useForm<FormValues>();
  const { message } = App.useApp();

  useEffect(() => {
    if (open) {
      // If initial values provided, use them; otherwise load from store
      if (initialAccessKeyId || initialSecretAccessKey) {
        form.setFieldsValue({
          accessKeyId: initialAccessKeyId || '',
          secretAccessKey: initialSecretAccessKey || '',
        });
      } else {
        // Load from store
        Store.load('r2-config.json')
          .then((store) => store.get<R2Config>('config'))
          .then((config) => {
            if (config) {
              form.setFieldsValue({
                accessKeyId: config.accessKeyId || '',
                secretAccessKey: config.secretAccessKey || '',
              });
            }
          })
          .catch(console.error);
      }
    }
  }, [open, initialAccessKeyId, initialSecretAccessKey, form]);

  async function handleSubmit(values: FormValues) {
    setSaving(true);
    try {
      const store = await Store.load('r2-config.json');
      const currentConfig = await store.get<R2Config>('config');

      const updatedConfig = {
        ...currentConfig,
        accessKeyId: values.accessKeyId,
        secretAccessKey: values.secretAccessKey,
      };
      await store.set('config', updatedConfig);
      await store.save();

      message.success('S3 credentials saved');
      onSave(values.accessKeyId, values.secretAccessKey);
      onClose();
    } catch (e) {
      console.error('Failed to save S3 credentials:', e);
      message.error('Failed to save credentials');
    } finally {
      setSaving(false);
    }
  }

  return (
    <Modal open={open} onCancel={onClose} footer={null} width={400} centered destroyOnHidden>
      <div style={{ textAlign: 'center', marginBottom: 24 }}>
        <KeyOutlined style={{ fontSize: 40, color: '#f6821f' }} />
        <h3 style={{ marginTop: 12, marginBottom: 4 }}>S3 API Credentials</h3>
        <p style={{ color: '#666', margin: 0 }}>Required for generating signed URLs</p>
      </div>

      <Form form={form} layout="vertical" onFinish={handleSubmit} autoComplete="off">
        <Form.Item
          label={
            <span>
              Access Key ID{' '}
              <Tooltip title="Click to create R2 API tokens">
                <QuestionCircleOutlined
                  style={{ color: '#1677ff', cursor: 'pointer' }}
                  onClick={() => openUrl('https://dash.cloudflare.com/?to=/:account/r2/api-tokens')}
                />
              </Tooltip>
            </span>
          }
          name="accessKeyId"
          rules={[{ required: true, message: 'Required' }]}
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

        <Form.Item style={{ marginBottom: 0 }}>
          <Button type="primary" htmlType="submit" loading={saving} block>
            Save
          </Button>
        </Form.Item>
      </Form>
    </Modal>
  );
}
