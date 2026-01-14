'use client';

import { useEffect, useState, useRef } from 'react';
import { Spin, Typography, Button, Space, Tooltip } from 'antd';
import { FileTextOutlined, FormatPainterOutlined, SaveOutlined } from '@ant-design/icons';
import { fetch } from '@tauri-apps/plugin-http';
import Editor, { type Monaco } from '@monaco-editor/react';
import type { editor } from 'monaco-editor';
import DownloadProgress from './DownloadProgress';
import { useThemeStore } from '../../stores/themeStore';

const { Text } = Typography;

interface TextViewerProps {
  url: string;
  filename?: string;
  maxHeight?: string;
  editable?: boolean;
  onLoadSuccess?: () => void;
  onLoadError?: (error: Error) => void;
  onSave?: (content: string) => Promise<void>;
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
  editable = false,
  onLoadSuccess,
  onLoadError,
  onSave,
}: TextViewerProps) {
  const [content, setContent] = useState<string | null>(null);
  const [editedContent, setEditedContent] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [downloadProgress, setDownloadProgress] = useState<{
    loaded: number;
    total: number | null;
  }>({ loaded: 0, total: null });

  const editorRef = useRef<editor.IStandaloneCodeEditor | null>(null);
  const monacoRef = useRef<Monaco | null>(null);

  const language = getLanguageFromExtension(filename);
  const appTheme = useThemeStore((s) => s.theme);
  const monacoTheme = appTheme === 'dark' ? 'vs-dark' : 'light';

  const isModified = editedContent !== null && editedContent !== content;

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

  // Reset edited content when original content changes
  useEffect(() => {
    setEditedContent(null);
  }, [content]);

  const handleEditorDidMount = (editor: editor.IStandaloneCodeEditor, monaco: Monaco) => {
    editorRef.current = editor;
    monacoRef.current = monaco;
  };

  const handleFormat = async () => {
    if (editorRef.current) {
      await editorRef.current.getAction('editor.action.formatDocument')?.run();
    }
  };

  const handleSave = async () => {
    if (!onSave || editedContent === null) return;

    setSaving(true);
    try {
      await onSave(editedContent);
      // Update original content to match saved content
      setContent(editedContent);
    } finally {
      setSaving(false);
    }
  };

  const handleEditorChange = (value: string | undefined) => {
    if (editable && value !== undefined) {
      setEditedContent(value);
    }
  };

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

  const displayContent = editedContent ?? content;
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
          {isModified && <Text className="text-sm text-orange-500">• Modified</Text>}
        </div>
        <div className="flex items-center gap-2">
          <Text className={`text-sm ${isDark ? 'text-gray-400' : 'text-gray-600'}`}>
            {(new TextEncoder().encode(displayContent).length / 1024).toFixed(1)} KB
          </Text>
          {editable && (
            <Space size="small">
              <Tooltip title="Format Document">
                <Button size="small" icon={<FormatPainterOutlined />} onClick={handleFormat} />
              </Tooltip>
              <Tooltip title={isModified ? 'Save Changes' : 'No changes to save'}>
                <Button
                  size="small"
                  type={isModified ? 'primary' : 'default'}
                  icon={<SaveOutlined />}
                  onClick={handleSave}
                  loading={saving}
                  disabled={!isModified || !onSave}
                >
                  Save
                </Button>
              </Tooltip>
            </Space>
          )}
        </div>
      </div>
      <Editor
        height={heightNum - 40}
        language={language}
        value={displayContent}
        theme={monacoTheme}
        onMount={handleEditorDidMount}
        onChange={handleEditorChange}
        options={{
          readOnly: !editable,
          minimap: { enabled: lineCount > 100 },
          scrollBeyondLastLine: false,
          fontSize: 13,
          lineNumbers: 'on',
          renderLineHighlight: editable ? 'line' : 'none',
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
