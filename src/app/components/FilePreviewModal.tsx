'use client';

import { useState, useEffect } from 'react';
import { Modal, Button, Space, Typography, App, Image, Spin } from 'antd';
import {
  LinkOutlined,
  CopyOutlined,
  FileOutlined,
  FileImageOutlined,
  PlaySquareOutlined,
  FilePdfOutlined,
} from '@ant-design/icons';
import { openUrl } from '@tauri-apps/plugin-opener';
import dynamic from 'next/dynamic';
import { FileItem } from '../hooks/useR2Files';
import { generateSignedUrl } from '../lib/r2cache';

const PDFViewer = dynamic(() => import('./preview/PDFViewer'), { ssr: false });

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

interface FilePreviewModalProps {
  open: boolean;
  onClose: () => void;
  file: FileItem | null;
  publicDomain?: string;
  accountId?: string;
  bucket?: string;
  accessKeyId?: string;
  secretAccessKey?: string;
  onCredentialsUpdate?: () => void;
}

export default function FilePreviewModal({
  open: isOpen,
  onClose,
  file,
  publicDomain,
  accountId,
  bucket,
  accessKeyId,
  secretAccessKey,
}: FilePreviewModalProps) {
  const { message } = App.useApp();
  const [signedUrl, setSignedUrl] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const needsCredentials = !publicDomain && (!accessKeyId || !secretAccessKey);

  // Generate signed URL when modal opens
  useEffect(() => {
    if (!isOpen || !file) {
      setSignedUrl(null);
      return;
    }

    // If public domain is set, use it directly
    if (publicDomain) {
      const domain = publicDomain.replace(/\/$/, '');
      setSignedUrl(`https://${domain}/${file.key}`);
      return;
    }

    // If S3 credentials are available, generate signed URL
    if (accountId && bucket && accessKeyId && secretAccessKey) {
      setLoading(true);
      generateSignedUrl(accountId, bucket, file.key, accessKeyId, secretAccessKey)
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
  }, [isOpen, file, publicDomain, accountId, bucket, accessKeyId, secretAccessKey]);

  if (!file) return null;

  const fileUrl = signedUrl;

  const isImage = isImageFile(file.name);
  const isVideo = isVideoFile(file.name);
  const isPdf = isPdfFile(file.name);

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
      width={isPdf ? 800 : 480}
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
        ) : isImage ? (
          <FileImageOutlined style={{ fontSize: 40, color: '#f6821f' }} />
        ) : isVideo ? (
          <PlaySquareOutlined style={{ fontSize: 40, color: '#f6821f' }} />
        ) : isPdf ? (
          <FilePdfOutlined style={{ fontSize: 40, color: '#f6821f' }} />
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
