'use client';

import { useEffect, useRef } from 'react';
import { Spin } from 'antd';
import Editor, { type Monaco } from '@monaco-editor/react';
import type { editor } from 'monaco-editor';
import { configureMonacoLoader } from '@/app/components/preview/monacoAssets';
import { useThemeStore } from '@/app/stores/themeStore';

if (typeof window !== 'undefined') {
  configureMonacoLoader(window.location.href);
}

export interface CodeEditorHandle {
  /** Runs Monaco's formatter; false when the language has none. */
  format: () => Promise<boolean>;
  focus: () => void;
}

interface CodeEditorProps {
  value: string;
  language: string;
  height: number;
  onChange: (value: string) => void;
  handleRef?: { current: CodeEditorHandle | null };
}

/** Monaco, editable, with the app theme. Line breaks are always '\n' inside. */
export default function CodeEditor({
  value,
  language,
  height,
  onChange,
  handleRef,
}: CodeEditorProps) {
  const theme = useThemeStore((s) => s.theme);
  const editorRef = useRef<editor.IStandaloneCodeEditor | null>(null);

  useEffect(
    () => () => {
      if (handleRef) handleRef.current = null;
    },
    [handleRef]
  );

  const onMount = (instance: editor.IStandaloneCodeEditor, monaco: Monaco) => {
    editorRef.current = instance;
    instance.getModel()?.setEOL(monaco.editor.EndOfLineSequence.LF);
    instance.focus();
    if (handleRef) {
      handleRef.current = {
        format: async () => {
          const action = instance.getAction('editor.action.formatDocument');
          if (!action || !action.isSupported()) return false;
          await action.run();
          return true;
        },
        focus: () => instance.focus(),
      };
    }
  };

  return (
    <Editor
      height={height}
      language={language}
      value={value}
      theme={theme === 'dark' ? 'vs-dark' : 'light'}
      onMount={onMount}
      onChange={(next) => onChange(next ?? '')}
      options={{
        minimap: { enabled: value.length > 20000 },
        scrollBeyondLastLine: false,
        fontSize: 13,
        wordWrap: language === 'markdown' || language === 'plaintext' ? 'on' : 'off',
        tabSize: 2,
        detectIndentation: true,
        automaticLayout: true,
        padding: { top: 10, bottom: 10 },
        scrollbar: { verticalScrollbarSize: 10, horizontalScrollbarSize: 10 },
      }}
      loading={<Spin />}
    />
  );
}
