'use client';

import { useEffect, useState } from 'react';
import { Spin, Typography } from 'antd';
import { FileTextOutlined } from '@ant-design/icons';
import { fetch } from '@tauri-apps/plugin-http';
import Editor from '@monaco-editor/react';
import DownloadProgress from './DownloadProgress';
import { useThemeStore } from '../../stores/themeStore';

const { Text } = Typography;

interface TextViewerProps {
  url: string;
  filename?: string;
  maxHeight?: string;
  onLoadSuccess?: () => void;
  onLoadError?: (error: Error) => void;
}

// Common text file extensions
const CODE_EXTENSIONS = ['js', 'ts', 'jsx', 'tsx', 'css', 'scss', 'less', 'html', 'vue', 'svelte'];
const CONFIG_EXTENSIONS = ['json', 'yaml', 'yml', 'toml', 'xml', 'ini', 'env'];
const DOC_EXTENSIONS = ['txt', 'md', 'markdown', 'rst', 'log'];
const SCRIPT_EXTENSIONS = ['sh', 'bash', 'zsh', 'fish', 'ps1', 'bat', 'cmd'];
const DATA_EXTENSIONS = ['csv', 'sql'];

export const TEXT_EXTENSIONS = [
  ...CODE_EXTENSIONS,
  ...CONFIG_EXTENSIONS,
  ...DOC_EXTENSIONS,
  ...SCRIPT_EXTENSIONS,
  ...DATA_EXTENSIONS,
];

function getLanguageFromExtension(filename: string): string {
  const ext = filename.split('.').pop()?.toLowerCase() || '';
  const langMap: Record<string, string> = {
    js: 'javascript',
    ts: 'typescript',
    jsx: 'javascript',
    tsx: 'typescript',
    py: 'python',
    rb: 'ruby',
    go: 'go',
    rs: 'rust',
    java: 'java',
    kt: 'kotlin',
    swift: 'swift',
    cs: 'csharp',
    php: 'php',
    css: 'css',
    scss: 'scss',
    less: 'less',
    html: 'html',
    xml: 'xml',
    json: 'json',
    yaml: 'yaml',
    yml: 'yaml',
    toml: 'ini',
    md: 'markdown',
    markdown: 'markdown',
    sql: 'sql',
    sh: 'shell',
    bash: 'shell',
    zsh: 'shell',
    ini: 'ini',
    env: 'ini',
    csv: 'plaintext',
    txt: 'plaintext',
    log: 'plaintext',
  };
  return langMap[ext] || 'plaintext';
}

export default function TextViewer({
  url,
  filename = '',
  maxHeight = '700px',
  onLoadSuccess,
  onLoadError,
}: TextViewerProps) {
  const [content, setContent] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [downloadProgress, setDownloadProgress] = useState<{
    loaded: number;
    total: number | null;
  }>({ loaded: 0, total: null });

  const language = getLanguageFromExtension(filename);
  const appTheme = useThemeStore((s) => s.theme);
  const monacoTheme = appTheme === 'dark' ? 'vs-dark' : 'light';

  useEffect(() => {
    if (!url) {
      setContent(null);
      return;
    }

    let cancelled = false;
    setLoading(true);
    setError(null);
    setContent(null);
    setDownloadProgress({ loaded: 0, total: null });

    const fetchWithProgress = async () => {
      try {
        const response = await fetch(url);
        if (!response.ok) {
          throw new Error(`HTTP ${response.status}: ${response.statusText}`);
        }

        const contentLength = response.headers.get('content-length');
        const total = contentLength ? parseInt(contentLength, 10) : null;

        if (!response.body) {
          const text = await response.text();
          if (!cancelled) {
            setContent(text);
            onLoadSuccess?.();
          }
          return;
        }

        // Stream the response for progress tracking
        const reader = response.body.getReader();
        const chunks: Uint8Array[] = [];
        let loaded = 0;

        while (true) {
          const { done, value } = await reader.read();
          if (done) break;
          if (cancelled) {
            reader.cancel();
            return;
          }

          chunks.push(value);
          loaded += value.length;
          setDownloadProgress({ loaded, total });
        }

        if (!cancelled) {
          // Combine chunks and decode as text
          const combined = new Uint8Array(loaded);
          let offset = 0;
          for (const chunk of chunks) {
            combined.set(chunk, offset);
            offset += chunk.length;
          }
          const text = new TextDecoder('utf-8').decode(combined);
          setContent(text);
          onLoadSuccess?.();
        }
      } catch (err) {
        if (!cancelled) {
          console.error('Failed to fetch text:', err);
          const errorMsg = err instanceof Error ? err.message : 'Failed to load file';
          setError(errorMsg);
          onLoadError?.(err instanceof Error ? err : new Error(errorMsg));
        }
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    };

    fetchWithProgress();

    return () => {
      cancelled = true;
    };
  }, [url, onLoadSuccess, onLoadError]);

  if (loading) {
    return <DownloadProgress loaded={downloadProgress.loaded} total={downloadProgress.total} />;
  }

  if (error) {
    return (
      <div className="flex h-96 w-full items-center justify-center">
        <Text type="danger">Error: {error}</Text>
      </div>
    );
  }

  if (content === null) {
    return (
      <div className="flex h-96 w-full items-center justify-center">
        <FileTextOutlined style={{ fontSize: 48, color: '#999' }} />
      </div>
    );
  }

  const lineCount = content.split('\n').length;
  // Parse maxHeight to number for Monaco
  const heightNum = parseInt(maxHeight, 10) || 500;

  const isDark = appTheme === 'dark';

  return (
    <div
      className={`overflow-hidden rounded border ${isDark ? 'border-gray-700' : 'border-gray-300'}`}
    >
      <div
        className={`flex items-center justify-between border-b px-4 py-2 ${
          isDark ? 'border-gray-700 bg-gray-800' : 'border-gray-200 bg-gray-100'
        }`}
      >
        <Text className={`text-sm ${isDark ? 'text-gray-400' : 'text-gray-600'}`}>
          {lineCount} {lineCount === 1 ? 'line' : 'lines'} • {language}
        </Text>
        <Text className={`text-sm ${isDark ? 'text-gray-400' : 'text-gray-600'}`}>
          {(new TextEncoder().encode(content).length / 1024).toFixed(1)} KB
        </Text>
      </div>
      <Editor
        height={heightNum - 40}
        language={language}
        value={content}
        theme={monacoTheme}
        options={{
          readOnly: true,
          minimap: { enabled: lineCount > 100 },
          scrollBeyondLastLine: false,
          fontSize: 13,
          lineNumbers: 'on',
          renderLineHighlight: 'none',
          selectionHighlight: true,
          wordWrap: 'on',
          contextmenu: true,
          scrollbar: {
            verticalScrollbarSize: 10,
            horizontalScrollbarSize: 10,
          },
          padding: { top: 12, bottom: 12 },
        }}
        loading={
          <div
            className={`flex h-full w-full items-center justify-center ${isDark ? 'bg-gray-900' : 'bg-white'}`}
          >
            <Spin />
          </div>
        }
      />
    </div>
  );
}
