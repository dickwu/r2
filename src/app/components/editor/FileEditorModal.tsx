'use client';

import { useTranslation } from 'react-i18next';
import { useCallback, useDeferredValue, useEffect, useId, useMemo, useRef, useState } from 'react';
import dynamic from 'next/dynamic';
import { App, Button, Segmented, Spin, Tooltip } from 'antd';
import {
  CheckCircleFilled,
  DownloadOutlined,
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
import { decodeText, describeFormat, type TextFormat } from '@/app/components/editor/textCodec';
import {
  downloadEditedCopy,
  editedText,
  localFileName,
  saveFailure,
  type SaveFailureKind,
} from '@/app/components/editor/editedContent';
import { tauriCopyDependencies } from '@/app/components/editor/copyToDisk';
import { sniffDelimiter, type Delimiter } from '@/app/components/editor/delimited';
import {
  errorMessage,
  SAVE_TIMEOUTS,
  saveWithVersionCheck,
  withTimeout,
} from '@/app/components/editor/saveFlow';
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
import { useEditorDirtyStore } from '@/app/stores/editorDirtyStore';

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
  | { tone: 'error'; text: string; offerCopy: boolean };

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
  const { t, i18n } = useTranslation();
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
  // The size of the last save, so the header stops showing the size the file opened at.
  const [savedSize, setSavedSize] = useState<number | null>(null);
  const [status, setStatus] = useState<Status>({ tone: 'idle' });
  const [markdownView, setMarkdownView] = useState<MarkdownView>('split');
  const [delimitedView, setDelimitedView] = useState<DelimitedView>('table');
  const [viewport, setViewport] = useState({ width: 1280, height: 800 });
  const etagRef = useRef<string | null>(null);
  const codeRef = useRef<CodeEditorHandle | null>(null);
  const imageRef = useRef<ImageEditorHandle | null>(null);
  const confirming = useRef(false);
  const saving = status.tone === 'saving';
  // Learned from a 403 on save: there's no cheap way to know before trying.
  const [readOnly, setReadOnly] = useState(false);
  const [copying, setCopying] = useState(false);

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
    setSavedSize(null);
    (async () => {
      try {
        const url = await generateSignedUrl(configRef.current, key);
        const detected = await detectObjectType(url).catch(() => null);
        const bytes = await fetchPreviewBytes(url, { signal: controller.signal });
        if (controller.signal.aborted) return;
        if (bytes.length > editability.maxBytes) {
          setLoadError(
            t('editor.modal.tooLargeOpen', {
              size: formatBytes(bytes.length),
              max: formatBytes(editability.maxBytes),
            })
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
          setLoadError(t('editor.modal.loadFailed', { error: errorMessage(error) }));
        }
      }
    })();
    return () => controller.abort();
  }, [key, t]);

  const isText = editability.mode === 'text';
  const dirty = isText ? loaded !== null && text !== savedText : imageEdits > 0;
  const dirtyRef = useRef(dirty);
  dirtyRef.current = dirty;

  // The updater asks before a restart would drop these changes.
  const editorId = useId();
  const setEditorDirty = useEditorDirtyStore((state) => state.setDirty);
  useEffect(() => {
    setEditorDirty(editorId, dirty);
    return () => setEditorDirty(editorId, false);
  }, [editorId, dirty, setEditorDirty]);

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

  /** The edited file as bytes (and upload body), exactly as Save would store it. */
  const editedFile = useCallback(async (): Promise<{
    bytes: Uint8Array;
    body: string | Uint8Array;
  }> => {
    if (!loaded) throw new Error(t('editor.modal.stillLoading'));
    if (editability.mode === 'image') {
      if (!imageRef.current) throw new Error(t('editor.modal.imageStillLoading'));
      const bytes = await withTimeout(
        imageRef.current.encode(),
        SAVE_TIMEOUTS.encodeMs,
        t('editor.modal.encodeTimeout')
      );
      return { bytes, body: bytes };
    }
    return editedText(text, loaded.text!.format);
  }, [loaded, editability, text, t]);

  const fail = useCallback((kind: SaveFailureKind, text: string) => {
    const failure = saveFailure(kind, text);
    if (failure.readOnly) setReadOnly(true);
    setStatus({ tone: 'error', text: failure.text, offerCopy: failure.offerCopy });
  }, []);

  /** Writes the edited file to a local file the user picks; the editor keeps the edits. */
  const downloadCopy = useCallback(async () => {
    if (!loaded || copying) return;
    setCopying(true);
    try {
      const { bytes } = await editedFile();
      const outcome = await downloadEditedCopy(key, bytes, tauriCopyDependencies);
      if (outcome.status === 'saved') {
        message.success(t('editor.modal.copySaved', { name: localFileName(outcome.path) }));
      }
    } catch (error) {
      message.error(t('editor.modal.copyFailed', { error: errorMessage(error) }));
    } finally {
      setCopying(false);
    }
  }, [loaded, copying, editedFile, key, message, t]);

  const save = useCallback(
    async (force = false) => {
      if (!loaded || status.tone === 'saving') return;
      if (!force && !dirtyRef.current) return;
      setStatus({ tone: 'saving' });
      // Every expected failure is reported below; the catch only keeps an
      // unexpected throw from leaving the editor at "Saving…".
      try {
        let bytes: Uint8Array;
        let body: string | Uint8Array;
        try {
          ({ bytes, body } = await editedFile());
        } catch (error) {
          fail('encode', errorMessage(error));
          return;
        }
        if (bytes.length > editability.maxBytes) {
          fail(
            'size',
            t('editor.modal.tooLargeSave', {
              size: formatBytes(bytes.length),
              max: formatBytes(editability.maxBytes),
            })
          );
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
          fail('conflict', t('editor.modal.notSavedConflict'));
          if (confirming.current) return;
          confirming.current = true;
          modal.confirm({
            title: t('editor.modal.conflictTitle'),
            content: t('editor.modal.conflictContent'),
            okText: t('transferAcct.overwrite'),
            okButtonProps: { danger: true },
            cancelText: t('editor.modal.keepEditing'),
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
          fail(outcome.kind, outcome.message);
          return;
        }

        setReadOnly(false);
        etagRef.current =
          outcome.etag ??
          (await withTimeout(
            currentEtag(),
            SAVE_TIMEOUTS.checkMs,
            t('editor.modal.readTimeout')
          ).catch(() => null));
        if (editability.mode === 'image') {
          setImageRevision((r) => r + 1);
        } else {
          setSavedText(snapshot);
        }
        setSavedSize(bytes.length);
        setStatus({ tone: 'saved', at: new Date() });
        message.success(t('editor.modal.saved'));
        onSaved(bytes.length);
      } catch (error) {
        fail('other', errorMessage(error));
      }
    },
    [
      loaded,
      status.tone,
      editability,
      text,
      editedFile,
      fail,
      contentType,
      key,
      modal,
      message,
      onSaved,
      t,
    ]
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
      title: t('editor.modal.discardTitle'),
      content: t('editor.modal.discardContent', { name: file.name }),
      okText: t('editor.modal.discardOk'),
      okButtonProps: { danger: true },
      cancelText: t('editor.modal.keepEditing'),
      onOk: onClose,
      afterClose: () => {
        confirming.current = false;
      },
    });
  }, [saving, onClose, modal, file.name, t]);

  const formatDocument = async () => {
    if (editability.mode !== 'text') return;
    if (editability.flavor === 'json') {
      try {
        const formatted = JSON.stringify(JSON.parse(text), null, jsonIndent(text));
        setText(formatted + (text.endsWith('\n') ? '\n' : ''));
      } catch {
        message.warning(t('editor.modal.fixJson'));
      }
      return;
    }
    const formatted = await codeRef.current?.format();
    if (!formatted) message.info(t('editor.modal.noFormatter'));
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
        title={
          editability.flavor === 'json' ? t('editor.modal.reformat') : t('editor.modal.formatDoc')
        }
      >
        <Button size="small" icon={<FormatPainterOutlined />} onClick={formatDocument}>
          {t('editor.modal.format')}
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
              { value: 'write', label: t('editor.modal.write') },
              { value: 'split', label: t('editor.modal.split') },
              { value: 'preview', label: t('editor.modal.preview') },
            ]}
          />
        );
      case 'json':
        return (
          <>
            {formatButton}
            <output className={`file-editor-status ${jsonError ? 'is-error' : 'is-ok'}`}>
              {jsonError ? <WarningFilled /> : <CheckCircleFilled />}
              <span className="file-editor-status-text">
                {jsonError ?? t('editor.modal.validJson')}
              </span>
            </output>
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
                  { value: 'table', label: t('editor.modal.table'), disabled: !!tableReason },
                  { value: 'text', label: t('editor.modal.text') },
                ]}
              />
            </Tooltip>
            {delimiter === ';' && (
              <span className="file-editor-hint">{t('editor.modal.semicolons')}</span>
            )}
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
          <span className="file-editor-status">{t('editor.modal.downloadingLatest')}</span>
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
        <div className="file-editor-toolbar" role="toolbar" aria-label={t('editor.modal.tools')}>
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
    loaded ? formatBytes(savedSize ?? loaded.bytes.length) : null,
  ]
    .filter(Boolean)
    .join(', ');

  const statusLine = (() => {
    switch (status.tone) {
      case 'saving':
        return <span className="file-editor-status">{t('accountEdit.saving')}</span>;
      case 'error':
        return (
          <span className="file-editor-status is-error" role="alert">
            <WarningFilled /> <span className="file-editor-status-text">{status.text}</span>
            {status.offerCopy && !readOnly && (
              <button
                type="button"
                className="file-editor-inline-action"
                onClick={() => void downloadCopy()}
                disabled={copying}
              >
                {t('editor.modal.downloadEdited')}
              </button>
            )}
          </span>
        );
      case 'saved':
        if (!dirty) {
          return (
            <span className="file-editor-status">
              {t('editor.modal.savedAt', {
                time: status.at.toLocaleTimeString(i18n.language, {
                  hour: 'numeric',
                  minute: '2-digit',
                }),
              })}
            </span>
          );
        }
      // falls through: edited again since
      default:
        return dirty ? (
          <span className="file-editor-status">
            <span className="file-editor-dirty" aria-hidden /> {t('editor.modal.unsaved')}
          </span>
        ) : null;
    }
  })();

  const saveKey =
    typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform)
      ? '⌘S'
      : 'Ctrl+S';

  const copyLabel = copying ? t('editor.modal.savingCopy') : t('editor.modal.downloadEdited');
  const copyHint = t('editor.modal.copyHint');
  const footer = (
    <>
      <span className="left" aria-live="polite">
        {statusLine}
      </span>
      {readOnly ? (
        <>
          <button className="btn" onClick={requestClose} disabled={saving}>
            {t('common.close')}
          </button>
          <Tooltip title={t('editor.modal.readOnlyHint', { key: saveKey })}>
            <button
              className="btn"
              onClick={() => void save()}
              disabled={!dirty || saving || !loaded}
            >
              {saving ? t('accountEdit.saving') : t('editor.modal.trySaveAgain')}
            </button>
          </Tooltip>
          <Tooltip title={copyHint}>
            <button
              className="btn btn-primary"
              onClick={() => void downloadCopy()}
              disabled={!loaded || copying}
            >
              <DownloadOutlined />
              {copyLabel}
            </button>
          </Tooltip>
        </>
      ) : (
        <>
          <Tooltip title={copyHint}>
            <button
              className="btn"
              onClick={() => void downloadCopy()}
              disabled={!loaded || copying}
              aria-label={t('editor.modal.downloadEdited')}
            >
              <DownloadOutlined />
              {copying ? t('editor.modal.savingCopy') : t('editor.modal.downloadCopy')}
            </button>
          </Tooltip>
          <button className="btn" onClick={requestClose} disabled={saving}>
            {t('common.close')}
          </button>
          <Tooltip title={t('editor.modal.saveWithKey', { key: saveKey })}>
            <button
              className="btn btn-primary"
              onClick={() => void save()}
              disabled={!dirty || saving || !loaded}
            >
              {saving ? t('accountEdit.saving') : t('config.saveChanges')}
            </button>
          </Tooltip>
        </>
      )}
    </>
  );

  return (
    <Modal
      open
      onClose={requestClose}
      title={
        <span style={{ display: 'inline-flex', alignItems: 'center', gap: 8 }}>
          {file.name}
          {dirty && (
            <span className="file-editor-dirty">
              <span className="sr-only">{t('editor.modal.unsaved')}</span>
            </span>
          )}
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
