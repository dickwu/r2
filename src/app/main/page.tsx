'use client';

import { useEffect, useState } from 'react';
import { useRouter } from 'next/navigation';
import { Store } from '@tauri-apps/plugin-store';
import { Button, Card, Typography, Upload, Space, App } from 'antd';
import { InboxOutlined, SettingOutlined } from '@ant-design/icons';

const { Title, Text } = Typography;
const { Dragger } = Upload;

interface R2Config {
  accountId: string;
  token: string;
  bucket: string;
  publicDomain: string;
}

export default function Main() {
  const router = useRouter();
  const [config, setConfig] = useState<R2Config | null>(null);
  const [loading, setLoading] = useState(true);
  const { message } = App.useApp();

  useEffect(() => {
    loadConfig();
  }, []);

  async function loadConfig() {
    try {
      const store = await Store.load('r2-config.json');
      const savedConfig = await store.get<R2Config>('config');

      if (!savedConfig || !savedConfig.accountId || !savedConfig.token || !savedConfig.bucket) {
        router.replace('/');
        return;
      }

      setConfig(savedConfig);
    } catch (e) {
      console.error('Failed to load config:', e);
      router.replace('/');
    } finally {
      setLoading(false);
    }
  }

  async function handleLogout() {
    try {
      const store = await Store.load('r2-config.json');
      await store.delete('config');
      await store.save();
      router.replace('/');
    } catch (e) {
      console.error('Failed to clear config:', e);
      message.error('Failed to clear configuration');
    }
  }

  if (loading) {
    return (
      <div className="center-container">
        <Text>Loading...</Text>
      </div>
    );
  }

  return (
    <div className="main-container">
      <div className="header">
        <Title level={4} style={{ margin: 0 }}>
          R2 Uploader
        </Title>
        <Button icon={<SettingOutlined />} onClick={handleLogout}>
          Settings
        </Button>
      </div>

      <Card size="small" style={{ marginBottom: 16 }}>
        <Space direction="vertical" size={0}>
          <Text>
            <Text strong>Bucket:</Text> {config?.bucket}
          </Text>
          {config?.publicDomain && (
            <Text>
              <Text strong>Domain:</Text> {config.publicDomain}
            </Text>
          )}
        </Space>
      </Card>

      <Dragger
        multiple
        showUploadList
        style={{ padding: '20px 0' }}
        beforeUpload={() => {
          message.info('Upload functionality coming soon');
          return false;
        }}
      >
        <p className="ant-upload-drag-icon">
          <InboxOutlined style={{ color: '#f6821f' }} />
        </p>
        <p className="ant-upload-text">Click or drag files to upload</p>
        <p className="ant-upload-hint">Files will be uploaded to your R2 bucket</p>
      </Dragger>
    </div>
  );
}

