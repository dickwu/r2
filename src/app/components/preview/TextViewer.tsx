'use client';

import { useEffect, useState } from 'react';
import { Spin, Typography } from 'antd';
import { FileTextOutlined } from '@ant-design/icons';
import Editor from '@monaco-editor/react';
import DownloadProgress from '@/app/components/preview/DownloadProgress';
import {
  fetchPreviewBytes,
  PreviewFetchAbortedError,
} from '@/app/components/preview/fetchPreviewBytes';
import { configureMonacoLoader } from '@/app/components/preview/monacoAssets';
import { getFileExtension } from '@/app/components/preview/previewKind';
import { monacoLanguageFor } from '@/app/components/editor/editability';
import { useThemeStore } from '@/app/stores/themeStore';

const { Text } = Typography;

// Monaco's AMD build is staged by scripts/copy-monaco-assets.mjs into
// public/monaco-editor, which Next's export ships at <origin>/monaco-editor/vs.
// Configure the loader before any <Editor> mounts: unconfigured, it fetches a
// different Monaco from jsdelivr and the editor never loads offline.
if (typeof window !== 'undefined') {
  configureMonacoLoader(window.location.href);
}

interface TextViewerProps {
  url: string;
  filename?: string;
  /** Format to highlight the text as; defaults to the name's extension. */
  extension?: string;
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

export default function TextViewer({
  url,
  filename = '',
  extension,
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

  // Editing happens in the file editor (opened from the preview's Edit button).
  const language = monacoLanguageFor(filename, extension ?? getFileExtension(filename));
  const appTheme = useThemeStore((s) => s.theme);
  const monacoTheme = appTheme === 'dark' ? 'vs-dark' : 'light';

  useEffect(() => {
    if (!url) {
      setContent(null);
      return;
    }

    const controller = new AbortController();
    setLoading(true);
    setError(null);
    setContent(null);
    setDownloadProgress({ loaded: 0, total: null });

    fetchPreviewBytes(url, { signal: controller.signal, onProgress: setDownloadProgress })
      .then((bytes) => {
        if (controller.signal.aborted) return;
        setContent(new TextDecoder('utf-8').decode(bytes));
        onLoadSuccess?.();
      })
      .catch((err: unknown) => {
        if (controller.signal.aborted || err instanceof PreviewFetchAbortedError) return;
        console.error('Failed to fetch text:', err);
        const errorMsg = err instanceof Error ? err.message : 'Failed to load file';
        setError(errorMsg);
        onLoadError?.(err instanceof Error ? err : new Error(errorMsg));
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });

    return () => controller.abort();
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

  const displayContent = content;
  const lineCount = displayContent.split('\n').length;
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
        <div className="flex items-center gap-3">
          <Text className={`text-sm ${isDark ? 'text-gray-400' : 'text-gray-600'}`}>
            {lineCount} {lineCount === 1 ? 'line' : 'lines'} • {language}
          </Text>
        </div>
        <div className="flex items-center gap-2">
          <Text className={`text-sm ${isDark ? 'text-gray-400' : 'text-gray-600'}`}>
            {(new TextEncoder().encode(displayContent).length / 1024).toFixed(1)} KB
          </Text>
        </div>
      </div>
      <Editor
        height={heightNum - 40}
        language={language}
        value={displayContent}
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
