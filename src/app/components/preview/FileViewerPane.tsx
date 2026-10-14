'use client';

import { useEffect, useMemo, useState } from 'react';
import { Button, Typography } from 'antd';
import { CloudDownloadOutlined, FileOutlined } from '@ant-design/icons';
import FileViewer, { fileViewerFullPreset, type ViewerOptions } from '@file-viewer/react-full';
import DownloadProgress from '@/app/components/preview/DownloadProgress';
import {
  fetchPreviewBytes,
  PreviewFetchAbortedError,
  type PreviewDownloadProgress,
} from '@/app/components/preview/fetchPreviewBytes';
import { configureFileViewerAssetBase } from '@/app/components/preview/fileViewerAssets';
import { getFileExtension, toViewerFilename } from '@/app/components/preview/previewKind';
import { useThemeStore } from '@/app/stores/themeStore';
import { formatBytes } from '@/app/utils/formatBytes';

const { Text } = Typography;

// Workers, WASM and fonts are staged by scripts/copy-file-viewer-assets.mjs
// into public/file-viewer, which Next's export ships at <origin>/file-viewer/.
// Pin the base explicitly: the library's inference reads script URLs, and
// Next's /_next/static/chunks layout is not one it recognises.
if (typeof window !== 'undefined') {
  configureFileViewerAssetBase(window.location.href);
}

/** Every extension the bundled full preset can actually render. */
const SUPPORTED_EXTENSIONS: ReadonlySet<string> = (() => {
  const extensions = new Set<string>();
  for (const plugin of fileViewerFullPreset.renderers ?? []) {
    for (const definition of plugin.definitions ?? []) {
      for (const extension of definition.extensions) extensions.add(extension.toLowerCase());
    }
  }
  return extensions;
})();

export function isFileViewerSupported(filename: string): boolean {
  return SUPPORTED_EXTENSIONS.has(getFileExtension(filename));
}

/**
 * The viewer needs the whole object in memory, so objects above this size
 * (archives, disk images, design files…) wait for an explicit click instead
 * of downloading the moment they are opened or stepped onto with the arrow keys.
 */
export const AUTO_LOAD_LIMIT_BYTES = 100 * 1024 * 1024;

interface FileViewerPaneProps {
  url: string;
  filename: string;
  /** Object size in bytes when the listing knows it. */
  size?: number;
  /** CSS height of the viewer; the library needs an explicit one. */
  height: string;
}

export default function FileViewerPane({ url, filename, size, height }: FileViewerPaneProps) {
  const [file, setFile] = useState<File | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [progress, setProgress] = useState<PreviewDownloadProgress>({ loaded: 0, total: null });
  // The URL the user confirmed a large download for; cleared by moving to another file.
  const [confirmedUrl, setConfirmedUrl] = useState<string | null>(null);
  const appTheme = useThemeStore((s) => s.theme);
  const supported = isFileViewerSupported(filename);
  const isLarge = (size ?? 0) > AUTO_LOAD_LIMIT_BYTES;
  const shouldLoad = supported && (!isLarge || confirmedUrl === url);

  useEffect(() => {
    // Drop the previous object's bytes even when this one is not loaded, so a
    // prompt or placeholder never sits on top of stale state.
    setFile(null);
    setError(null);
    setProgress({ loaded: 0, total: null });
    if (!shouldLoad) return;
    const controller = new AbortController();

    fetchPreviewBytes(url, { signal: controller.signal, onProgress: setProgress })
      .then((bytes) => {
        if (controller.signal.aborted) return;
        setFile(new File([bytes as BlobPart], toViewerFilename(filename)));
      })
      .catch((err: unknown) => {
        if (controller.signal.aborted || err instanceof PreviewFetchAbortedError) return;
        console.error('Failed to fetch preview:', err);
        setError(err instanceof Error ? err.message : 'Failed to load file');
      });

    return () => controller.abort();
  }, [url, filename, shouldLoad]);

  // The component reloads whenever this object's identity changes.
  const options = useMemo<ViewerOptions>(
    () => ({
      theme: appTheme === 'dark' ? 'dark' : 'light',
      // Download, print and HTML export go through <a download> and
      // window.open(), which the Tauri webview has no handler for: the click
      // would do nothing or throw. The modal's Open/Copy URL covers downloads.
      toolbar: { position: 'bottom-right', download: false, print: false, exportHtml: false },
      archive: { entryActions: { download: false } },
    }),
    [appTheme]
  );

  if (!supported) {
    return (
      <div className="flex flex-col items-center justify-center gap-2 py-10">
        <FileOutlined style={{ fontSize: 40, color: 'var(--accent)' }} />
        <Text type="secondary" style={{ fontSize: 12.5 }}>
          No preview available for this file type
        </Text>
      </div>
    );
  }

  if (!shouldLoad) {
    return (
      <div className="flex flex-col items-center justify-center gap-3 py-10">
        <FileOutlined style={{ fontSize: 40, color: 'var(--accent)' }} />
        <Text type="secondary" style={{ fontSize: 12.5 }}>
          Previewing downloads the whole file ({formatBytes(size ?? 0)}) into memory
        </Text>
        <Button icon={<CloudDownloadOutlined />} onClick={() => setConfirmedUrl(url)}>
          Load preview
        </Button>
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex h-96 w-full items-center justify-center">
        <Text type="danger">Error: {error}</Text>
      </div>
    );
  }

  if (!file) {
    return <DownloadProgress loaded={progress.loaded} total={progress.total} filename={filename} />;
  }

  return (
    <div
      data-scrollable
      style={{ height, textAlign: 'left' }}
      className="overflow-hidden rounded-lg"
    >
      <FileViewer
        file={file}
        filename={file.name}
        type={getFileExtension(filename)}
        size={file.size}
        options={options}
      />
    </div>
  );
}
