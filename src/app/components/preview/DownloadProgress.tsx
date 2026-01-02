'use client';

import { Progress, Typography } from 'antd';
import { CloudDownloadOutlined } from '@ant-design/icons';

const { Text } = Typography;

interface DownloadProgressProps {
  loaded: number;
  total: number | null;
  filename?: string;
}

function formatBytes(bytes: number): string {
  if (bytes === 0) return '0 B';
  const k = 1024;
  const sizes = ['B', 'KB', 'MB', 'GB'];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return `${(bytes / Math.pow(k, i)).toFixed(1)} ${sizes[i]}`;
}

export default function DownloadProgress({ loaded, total, filename }: DownloadProgressProps) {
  const percent = total ? Math.round((loaded / total) * 100) : 0;
  const hasTotal = total !== null && total > 0;

  return (
    <div className="flex h-96 w-full flex-col items-center justify-center gap-4">
      <CloudDownloadOutlined style={{ fontSize: 48, color: '#1890ff' }} />
      {filename && <Text className="max-w-xs truncate text-white">{filename}</Text>}
      {hasTotal ? (
        <>
          <Progress
            type="circle"
            percent={percent}
            size={120}
            strokeColor={{ '0%': '#108ee9', '100%': '#87d068' }}
          />
          <Text className="text-gray-300">
            {formatBytes(loaded)} / {formatBytes(total)}
          </Text>
        </>
      ) : (
        <>
          <Progress type="circle" percent={0} size={120} status="active" />
          <Text className="text-gray-300">{formatBytes(loaded)} downloaded</Text>
        </>
      )}
    </div>
  );
}
