'use client';

import { useCallback, useDeferredValue, useEffect, useMemo, useRef, useState } from 'react';
import dynamic from 'next/dynamic';
import { App, Button, Segmented, Spin, Tooltip } from 'antd';
import {
  CheckCircleFilled,
  EditOutlined,
  FormatPainterOutlined,
  WarningFilled,
} from '@ant-design/icons';
import { generateSignedUrl, uploadContent, type StorageConfig } from '@/app/lib/r2cache';
import type { FileItem } from '@/app/hooks/useR2Files';
import { detectObjectType, fetchPreviewBytes } from '@/app/components/preview/fetchPreviewBytes';
import Modal from '@/app/components/ui/Modal';
import FullscreenButton from '@/app/components/ui/FullscreenButton';
import { useFullscreen } from '@/app/components/ui/useFullscreen';
import { guessContentType } from '@/app/components/editor/editability';
import {
  applyLineEnding,
  decodeText,
  describeFormat,
  encodeText,
  isPlainUtf8,
  type TextFormat,
} from '@/app/components/editor/textCodec';
import { sniffDelimiter, type Delimiter } from '@/app/components/editor/delimited';
import { errorMessage, saveWithVersionCheck } from '@/app/components/editor/saveFlow';
import {
  documentLabel,
  formatBytes,
  jsonIndent,
  jsonProblem,
  type EditableAs,
} from '@/app/components/editor/editorText';
import type { CodeEditorHandle } from '@/app/components/editor/CodeEditor';
import type { ImageEditorHandle } from '@/app/components/editor/ImageEditor';
import { tableUnavailableReason } from '@/app/components/editor/DelimitedTableEditor';

const editorLoading = () => (
  <div style={{ display: 'grid', placeItems: 'center', height: '100%', minHeight: 120 }}>
    <Spin />
  </div>
);
const CodeEditor = dynamic(() => import('@/app/components/editor/CodeEditor'), {
  ssr: false,
  loading: editorLoading,
});
const MarkdownPreview = dynamic(() => import('@/app/components/editor/MarkdownPreview'), {
  ssr: false,
  loading: editorLoading,
});
const DelimitedTableEditor = dynamic(() => import('@/app/components/editor/DelimitedTableEditor'), {
  ssr: false,
  loading: editorLoading,
});
const ImageEditor = dynamic(() => import('@/app/components/editor/ImageEditor'), {
  ssr: false,
  loading: editorLoading,
});

type MarkdownView = 'write' | 'split' | 'preview';
type DelimitedView = 'table' | 'text';
type Status =
  | { tone: 'idle' }
  | { tone: 'saving' }
  | { tone: 'saved'; at: Date }
  | { tone: 'error'; text: string };

interface Loaded {
  bytes: Uint8Array;
  etag: string | null;
  contentType: string | null;
  text?: { value: string; format: TextFormat };
}

interface FileEditorModalProps {
  config: StorageConfig;
  file: FileItem;
  editability: EditableAs;
  /** The Content-Type the preview identified, used when the object reports none. */
  previewContentType?: string;
  /** After a save: the new size of the object. */
  onSaved: (size: number) => void;
  onClose: () => void;
}

/**
 * The editor for one object. It downloads the object fresh (not from the
 * preview), remembers the version it opened, and saves back over exactly that
 * version: a newer one on the server is a conflict the user settles.
 */
export default function FileEditorModal({
  config,
  file,
  editability,
  previewContentType,
  onSaved,
  onClose,
}: FileEditorModalProps) {
  const { message, modal } = App.useApp();
  const { fullscreen, toggle } = useFullscreen({ active: true, shortcut: false });
  const key = file.key;
  const configRef = useRef(config);
  configRef.current = config;
  const editabilityRef = useRef(editability);

  const [loaded, setLoaded] = useState<Loaded | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [text, setText] = useState('');
  const [savedText, setSavedText] = useState('');
  const [imageEdits, setImageEdits] = useState(0);
  const [imageRevision, setImageRevision] = useState(0);
  const [status, setStatus] = useState<Status>({ tone: 'idle' });
  const [markdownView, setMarkdownView] = useState<MarkdownView>('split');
  const [delimitedView, setDelimitedView] = useState<DelimitedView>('table');
  const [viewport, setViewport] = useState({ width: 1280, height: 800 });
  const etagRef = useRef<string | null>(null);
  const codeRef = useRef<CodeEditorHandle | null>(null);
  const imageRef = useRef<ImageEditorHandle | null>(null);
  const confirming = useRef(false);
  const saving = status.tone === 'saving';

  useEffect(() => {
    const update = () => setViewport({ width: window.innerWidth, height: window.innerHeight });
    update();
    window.addEventListener('resize', update);
    return () => window.removeEventListener('resize', update);
  }, []);

  // Load the object once per key. The ETag is read before the bytes: if the
  // object changes in between, the next save reports a conflict rather than
  // silently overwriting the newer version.
  useEffect(() => {
    const editability = editabilityRef.current;
    const controller = new AbortController();
    setLoaded(null);
    setLoadError(null);
    (async () => {
      try {
        const url = await generateSignedUrl(configRef.current, key);
        const detected = await detectObjectType(url).catch(() => null);
        const bytes = await fetchPreviewBytes(url, { signal: controller.signal });
        if (controller.signal.aborted) return;
        if (bytes.length > editability.maxBytes) {
          setLoadError(
            `This file is ${formatBytes(bytes.length)}. The editor opens files up to ${formatBytes(editability.maxBytes)}; download it to edit it locally.`
          );
          return;
        }
        const next: Loaded = {
          bytes,
          etag: detected?.etag ?? null,
          contentType: detected?.contentType ?? null,
        };
        if (editability.mode === 'text') {
          const decoded = decodeText(bytes);
          if (!decoded.ok) {
            setLoadError(decoded.reason);
            return;
          }
          next.text = { value: decoded.text, format: decoded.format };
          setText(decoded.text);
          setSavedText(decoded.text);
          if (
            editability.flavor === 'delimited' &&
            tableUnavailableReason(decoded.text, editability.delimiter ?? ',')
          ) {
            setDelimitedView('text');
          }
        }
        etagRef.current = next.etag;
        setLoaded(next);
      } catch (error) {
        if (!controller.signal.aborted) {
          setLoadError(`The file could not be downloaded for editing: ${errorMessage(error)}`);
        }
      }
    })();
    return () => controller.abort();
  }, [key]);

  const isText = editability.mode === 'text';
  const dirty = isText ? loaded !== null && text !== savedText : imageEdits > 0;
  const dirtyRef = useRef(dirty);
  dirtyRef.current = dirty;

  const delimiter: Delimiter = useMemo(() => {
    if (editability.mode !== 'text' || editability.flavor !== 'delimited') return ',';
    const declared = editability.delimiter ?? ',';
    return declared === '\t' ? declared : sniffDelimiter(savedText, declared);
  }, [editability, savedText]);

  const deferredText = useDeferredValue(text);
  const jsonError = useMemo(
    () => (isText && editability.flavor === 'json' ? jsonProblem(deferredText) : null),
    [isText, editability, deferredText]
  );
  const tableReason = useMemo(
    () =>
      isText && editability.flavor === 'delimited'
        ? tableUnavailableReason(deferredText, delimiter)
        : null,
    [isText, editability, deferredText, delimiter]
  );

  const contentType = useCallback((): string => {
    const stored = loaded?.contentType?.trim();
    if (editability.mode === 'image') {
      return stored && stored.startsWith('image/') ? stored : editability.mime;
    }
    return stored || previewContentType || guessContentType(file.name);
  }, [loaded, editability, previewContentType, file.name]);

  const save = useCallback(
    async (force = false) => {
      if (!loaded || status.tone === 'saving') return;
      if (!force && !dirtyRef.current) return;
      setStatus({ tone: 'saving' });

      let bytes: Uint8Array;
      let body: string | Uint8Array;
      try {
        if (editability.mode === 'image') {
          if (!imageRef.current) throw new Error('The image is still loading');
          bytes = await imageRef.current.encode();
          body = bytes;
        } else {
          const format = loaded.text!.format;
          bytes = encodeText(text, format);
          body = isPlainUtf8(format) ? applyLineEnding(text, format.eol) : bytes;
        }
      } catch (error) {
        setStatus({ tone: 'error', text: errorMessage(error) });
        return;
      }
      if (bytes.length > editability.maxBytes) {
        setStatus({
          tone: 'error',
          text: `The edited file is ${formatBytes(bytes.length)}, over the ${formatBytes(editability.maxBytes)} the editor saves. Undo some changes.`,
        });
        return;
      }

      const snapshot = text;
      const type = contentType();
      const currentEtag = async () => {
        const url = await generateSignedUrl(configRef.current, key);
        return (await detectObjectType(url)).etag ?? null;
      };
      const outcome = await saveWithVersionCheck(
        etagRef.current,
        {
          currentEtag,
          upload: (ifMatch) =>
            uploadContent(configRef.current, key, body, type, ifMatch ? { ifMatch } : undefined),
        },
        { force }
      );

      if (outcome.status === 'conflict') {
        setStatus({ tone: 'error', text: 'Not saved: the file changed on the server.' });
        if (confirming.current) return;
        confirming.current = true;
        modal.confirm({
          title: 'This file changed on the server',
          content:
            'A newer version was saved after you opened it. Overwrite it with your version, or keep editing and copy your changes somewhere safe first.',
          okText: 'Overwrite',
          okButtonProps: { danger: true },
          cancelText: 'Keep editing',
          onOk: () => {
            void save(true);
          },
          afterClose: () => {
            confirming.current = false;
          },
        });
        return;
      }
      if (outcome.status === 'error') {
        setStatus({ tone: 'error', text: outcome.message });
        return;
      }

      etagRef.current = outcome.etag ?? (await currentEtag().catch(() => null));
      if (editability.mode === 'image') {
        setImageRevision((r) => r + 1);
      } else {
        setSavedText(snapshot);
      }
      setStatus({ tone: 'saved', at: new Date() });
      message.success('Changes saved');
      onSaved(bytes.length);
    },
    [loaded, status.tone, editability, text, contentType, key, modal, message, onSaved]
  );

  // Cmd/Ctrl+S, caught before Monaco or the webview sees it.
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && !event.altKey && event.key.toLowerCase() === 's') {
        event.preventDefault();
        event.stopPropagation();
        void save();
      }
    };
    window.addEventListener('keydown', onKeyDown, true);
    return () => window.removeEventListener('keydown', onKeyDown, true);
  }, [save]);

  const requestClose = useCallback(() => {
    if (saving) return;
    if (!dirtyRef.current) {
      onClose();
      return;
    }
    if (confirming.current) return;
    confirming.current = true;
    modal.confirm({
      title: 'Discard unsaved changes?',
      content: `Your changes to ${file.name} have not been saved.`,
      okText: 'Discard changes',
      okButtonProps: { danger: true },
      cancelText: 'Keep editing',
      onOk: onClose,
      afterClose: () => {
        confirming.current = false;
      },
    });
  }, [saving, onClose, modal, file.name]);

  const formatDocument = async () => {
    if (editability.mode !== 'text') return;
    if (editability.flavor === 'json') {
      try {
        const formatted = JSON.stringify(JSON.parse(text), null, jsonIndent(text));
        setText(formatted + (text.endsWith('\n') ? '\n' : ''));
      } catch {
        message.warning('Fix the JSON error before formatting.');
      }
      return;
    }
    const formatted = await codeRef.current?.format();
    if (!formatted) message.info('There is no formatter for this language.');
  };

  // Space for the document: the window, less the header, toolbar and footer.
  const areaHeight = Math.max(
    240,
    Math.round(fullscreen ? viewport.height - 172 : Math.min(viewport.height * 0.86 - 214, 720))
  );

  const toolbar = (() => {
    if (editability.mode !== 'text') return null;
    const formatButton = (
      <Tooltip
        title={editability.flavor === 'json' ? 'Reformat with the same indent' : 'Format document'}
      >
        <Button size="small" icon={<FormatPainterOutlined />} onClick={formatDocument}>
          Format
        </Button>
      </Tooltip>
    );
    switch (editability.flavor) {
      case 'markdown':
        return (
          <Segmented<MarkdownView>
            size="small"
            value={markdownView}
            onChange={setMarkdownView}
            options={[
              { value: 'write', label: 'Write' },
              { value: 'split', label: 'Side by side' },
              { value: 'preview', label: 'Preview' },
            ]}
          />
        );
      case 'json':
        return (
          <>
            {formatButton}
            <span
              className={`file-editor-status ${jsonError ? 'is-error' : 'is-ok'}`}
              role="status"
            >
              {jsonError ? <WarningFilled /> : <CheckCircleFilled />}
              {jsonError ?? 'Valid JSON'}
            </span>
          </>
        );
      case 'delimited':
        return (
          <>
            <Tooltip title={tableReason ?? undefined}>
              <Segmented<DelimitedView>
                size="small"
                value={tableReason ? 'text' : delimitedView}
                onChange={setDelimitedView}
                options={[
                  { value: 'table', label: 'Table', disabled: !!tableReason },
                  { value: 'text', label: 'Text' },
                ]}
              />
            </Tooltip>
            {delimiter === ';' && <span className="file-editor-hint">Separated by semicolons</span>}
          </>
        );
      default:
        return formatButton;
    }
  })();

  const codePane = (height: number) =>
    editability.mode === 'text' ? (
      <div className="file-editor-pane" style={{ height }}>
        <CodeEditor
          value={text}
          language={editability.language}
          height={height - 2}
          onChange={setText}
          handleRef={codeRef}
        />
      </div>
    ) : null;

  const body = (() => {
    if (loadError) return <p className="file-editor-message">{loadError}</p>;
    if (!loaded) {
      return (
        <div
          style={{
            display: 'grid',
            placeItems: 'center',
            alignContent: 'center',
            gap: 12,
            height: areaHeight,
          }}
        >
          <Spin />
          <span className="file-editor-status">Downloading the latest version</span>
        </div>
      );
    }
    if (editability.mode === 'image') {
      return (
        <ImageEditor
          bytes={loaded.bytes}
          mime={editability.mime}
          height={areaHeight + 38}
          onEditsChange={setImageEdits}
          handleRef={imageRef}
          savedRevision={imageRevision}
        />
      );
    }
    let content: React.ReactNode;
    if (editability.flavor === 'markdown') {
      const preview = (
        <div className="file-editor-pane" style={{ height: areaHeight }}>
          <MarkdownPreview source={text} height={areaHeight - 2} />
        </div>
      );
      content =
        markdownView === 'write' ? (
          codePane(areaHeight)
        ) : markdownView === 'preview' ? (
          preview
        ) : (
          <div className="file-editor-split">
            {codePane(areaHeight)}
            {preview}
          </div>
        );
    } else if (editability.flavor === 'delimited' && !tableReason && delimitedView === 'table') {
      content = (
        <DelimitedTableEditor
          text={text}
          delimiter={delimiter}
          height={areaHeight}
          onChange={setText}
        />
      );
    } else {
      content = codePane(areaHeight);
    }
    return (
      <div className="file-editor">
        <div className="file-editor-toolbar" role="toolbar" aria-label="Editor tools">
          {toolbar}
        </div>
        {content}
      </div>
    );
  })();

  const format = loaded?.text?.format;
  const subtitle = [
    documentLabel(editability),
    format ? describeFormat(format) : null,
    loaded ? formatBytes(loaded.bytes.length) : null,
  ]
    .filter(Boolean)
    .join(', ');

  const statusLine = (() => {
    switch (status.tone) {
      case 'saving':
        return <span className="file-editor-status">Saving…</span>;
      case 'error':
        return (
          <span className="file-editor-status is-error" role="alert">
            <WarningFilled /> {status.text}
          </span>
        );
      case 'saved':
        if (!dirty) {
          return (
            <span className="file-editor-status">
              Saved at{' '}
              {status.at.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' })}
            </span>
          );
        }
      // falls through: edited again since
      default:
        return dirty ? (
          <span className="file-editor-status">
            <span className="file-editor-dirty" aria-hidden /> Unsaved changes
          </span>
        ) : null;
    }
  })();

  const saveKey =
    typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform)
      ? '⌘S'
      : 'Ctrl+S';

  const footer = (
    <>
      <span className="left" aria-live="polite">
        {statusLine}
      </span>
      <button className="btn" onClick={requestClose} disabled={saving}>
        Close
      </button>
      <Tooltip title={`Save changes (${saveKey})`}>
        <button
          className="btn btn-primary"
          onClick={() => void save()}
          disabled={!dirty || saving || !loaded}
        >
          {saving ? 'Saving…' : 'Save changes'}
        </button>
      </Tooltip>
    </>
  );

  return (
    <Modal
      open
      onClose={requestClose}
      title={
        <span style={{ display: 'inline-flex', alignItems: 'center', gap: 8 }}>
          {file.name}
          {dirty && <span className="file-editor-dirty" role="img" aria-label="Unsaved changes" />}
        </span>
      }
      subtitle={subtitle || undefined}
      icon={<EditOutlined style={{ fontSize: 18 }} />}
      width={Math.min(1180, Math.round(viewport.width * 0.94))}
      fullscreen={fullscreen}
      headerActions={<FullscreenButton fullscreen={fullscreen} onToggle={toggle} />}
      footer={footer}
    >
      {body}
    </Modal>
  );
}
