'use client';

import { useState, useEffect, useCallback } from 'react';
import { Modal, Button, Space, Typography, App, Image, Spin } from 'antd';
import {
  LinkOutlined,
  CopyOutlined,
  FileOutlined,
  FileImageOutlined,
  PlaySquareOutlined,
  FilePdfOutlined,
  FileTextOutlined,
} from '@ant-design/icons';
import { openUrl } from '@tauri-apps/plugin-opener';
import dynamic from 'next/dynamic';
import { FileItem } from '@/app/hooks/useR2Files';
import { generateSignedUrl, uploadContent, StorageConfig } from '@/app/lib/r2cache';
import { TEXT_EXTENSIONS } from '@/app/components/preview/TextViewer';

const PDFViewer = dynamic(() => import('@/app/components/preview/PDFViewer'), { ssr: false });
const TextViewer = dynamic(() => import('@/app/components/preview/TextViewer'), { ssr: false });

const { Text, Paragraph } = Typography;

const IMAGE_EXTENSIONS = ['jpg', 'jpeg', 'png', 'gif', 'webp', 'svg', 'bmp', 'ico'];
const VIDEO_EXTENSIONS = ['mp4', 'webm', 'ogg', 'mov', 'm4v'];
const PDF_EXTENSIONS = ['pdf'];

function getFileExtension(filename: string): string {
  return filename.split('.').pop()?.toLowerCase() || '';
}

function isImageFile(filename: string): boolean {
  return IMAGE_EXTENSIONS.includes(getFileExtension(filename));
}

function isVideoFile(filename: string): boolean {
  return VIDEO_EXTENSIONS.includes(getFileExtension(filename));
}

function isPdfFile(filename: string): boolean {
  return PDF_EXTENSIONS.includes(getFileExtension(filename));
}

function isTextFile(filename: string): boolean {
  return TEXT_EXTENSIONS.includes(getFileExtension(filename));
}

// Map file extensions to MIME types for text files
const TEXT_CONTENT_TYPES: Record<string, string> = {
  js: 'application/javascript',
  ts: 'application/typescript',
  jsx: 'text/jsx',
  tsx: 'text/tsx',
  css: 'text/css',
  scss: 'text/x-scss',
  less: 'text/x-less',
  html: 'text/html',
  xml: 'application/xml',
  json: 'application/json',
  yaml: 'text/yaml',
  yml: 'text/yaml',
  toml: 'text/x-toml',
  md: 'text/markdown',
  markdown: 'text/markdown',
  sql: 'text/x-sql',
  sh: 'text/x-shellscript',
  bash: 'text/x-shellscript',
  zsh: 'text/x-shellscript',
  txt: 'text/plain',
  log: 'text/plain',
  csv: 'text/csv',
  ini: 'text/x-ini',
  env: 'text/plain',
};

function getContentType(filename: string): string {
  const ext = filename.split('.').pop()?.toLowerCase() || '';
  return TEXT_CONTENT_TYPES[ext] || 'text/plain';
}

interface FilePreviewModalProps {
  open: boolean;
  onClose: () => void;
  file: FileItem | null;
  config?: StorageConfig | null;
  onCredentialsUpdate?: () => void;
  onFileUpdated?: () => void;
}

export default function FilePreviewModal({
  open: isOpen,
  onClose,
  file,
  config,
  onFileUpdated,
}: FilePreviewModalProps) {
  const { message } = App.useApp();
  const [signedUrl, setSignedUrl] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const hasCredentials =
    !!config?.accessKeyId &&
    !!config?.secretAccessKey &&
    (config.provider !== 'aws' || !!config.region) &&
    (config.provider !== 'minio' || (!!config.endpointHost && !!config.endpointScheme)) &&
    (config.provider !== 'rustfs' || (!!config.endpointHost && !!config.endpointScheme));
  const needsCredentials = !config?.publicDomain && !hasCredentials;
  const canEdit = !!(config?.bucket && hasCredentials);

  const handleSaveContent = useCallback(
    async (content: string) => {
      if (!file || !config || !hasCredentials) {
        throw new Error('Missing credentials or file info');
      }

      try {
        await uploadContent(config, file.key, content, getContentType(file.name));
        message.success('File saved successfully');
        onFileUpdated?.();
      } catch (err) {
        console.error('Failed to save file:', err);
        message.error('Failed to save file');
        throw err;
      }
    },
    [file, config, hasCredentials, message, onFileUpdated]
  );

  // Generate signed URL when modal opens
  useEffect(() => {
    if (!isOpen || !file) {
      setSignedUrl(null);
      return;
    }

    // If public domain is set, use it directly
    if (config?.publicDomain) {
      const domain = config.publicDomain.replace(/\/$/, '');
      const scheme = config.publicDomainScheme || 'https';
      setSignedUrl(`${scheme}://${domain}/${file.key}`);
      return;
    }

    // If S3 credentials are available, generate signed URL
    if (config && hasCredentials) {
      setLoading(true);
      generateSignedUrl(config, file.key)
        .then(setSignedUrl)
        .catch((e) => {
          console.error('Failed to generate signed URL:', e);
          setSignedUrl(null);
        })
        .finally(() => setLoading(false));
      return;
    }

    // Fallback: no URL available
    setSignedUrl(null);
  }, [isOpen, file, config, hasCredentials]);

  if (!file) return null;

  const fileUrl = signedUrl;

  const isImage = isImageFile(file.name);
  const isVideo = isVideoFile(file.name);
  const isPdf = isPdfFile(file.name);
  const isText = isTextFile(file.name);

  async function handleOpenUrl() {
    if (!fileUrl) {
      message.warning('No public domain configured');
      return;
    }
    try {
      await openUrl(fileUrl);
    } catch (e) {
      console.error('Failed to open URL:', e);
      message.error('Failed to open URL');
    }
  }

  async function handleCopyUrl() {
    if (!fileUrl) {
      message.warning('No public domain configured');
      return;
    }
    try {
      await navigator.clipboard.writeText(fileUrl);
      message.success('URL copied to clipboard');
    } catch (e) {
      console.error('Failed to copy URL:', e);
      message.error('Failed to copy URL');
    }
  }

  return (
    <Modal
      open={isOpen}
      onCancel={onClose}
      title={file.name}
      footer={null}
      width={'85%'}
      centered
      destroyOnHidden
    >
      <div style={{ textAlign: 'center', marginBottom: 24 }}>
        {loading ? (
          <Spin />
        ) : isImage && fileUrl ? (
          <Image src={fileUrl} alt={file.name} style={{ maxHeight: 300, objectFit: 'contain' }} />
        ) : isVideo && fileUrl ? (
          <video
            src={fileUrl}
            controls
            style={{ maxWidth: '100%', maxHeight: 300, borderRadius: 8 }}
          />
        ) : isPdf && fileUrl ? (
          <PDFViewer url={fileUrl} showControls maxHeight="500px" />
        ) : isText && fileUrl ? (
          <TextViewer
            url={fileUrl}
            filename={file.name}
            maxHeight="500px"
            editable={canEdit}
            onSave={handleSaveContent}
          />
        ) : isImage ? (
          <FileImageOutlined style={{ fontSize: 40, color: '#f6821f' }} />
        ) : isVideo ? (
          <PlaySquareOutlined style={{ fontSize: 40, color: '#f6821f' }} />
        ) : isPdf ? (
          <FilePdfOutlined style={{ fontSize: 40, color: '#f6821f' }} />
        ) : isText ? (
          <FileTextOutlined style={{ fontSize: 40, color: '#f6821f' }} />
        ) : (
          <FileOutlined style={{ fontSize: 40, color: '#f6821f' }} />
        )}
      </div>

      {fileUrl ? (
        <Paragraph
          style={{
            background: 'var(--ant-color-fill-tertiary)',
            padding: '12px 16px',
            borderRadius: 8,
            marginBottom: 24,
            wordBreak: 'break-all',
          }}
        >
          <Text copyable={{ text: fileUrl }}>{fileUrl}</Text>
        </Paragraph>
      ) : needsCredentials ? (
        <div style={{ textAlign: 'center', marginBottom: 24 }}>
          <Paragraph type="secondary">
            S3 credentials required for signed URL. Configure in Account Settings.
          </Paragraph>
        </div>
      ) : (
        <Paragraph type="secondary" style={{ textAlign: 'center', marginBottom: 24 }}>
          Unable to generate file URL
        </Paragraph>
      )}

      <Space style={{ width: '100%', justifyContent: 'center' }}>
        <Button icon={<LinkOutlined />} onClick={handleOpenUrl} disabled={!fileUrl}>
          Open URL
        </Button>
        <Button type="primary" icon={<CopyOutlined />} onClick={handleCopyUrl} disabled={!fileUrl}>
          Copy URL
        </Button>
      </Space>
    </Modal>
  );
}
