'use client';

import { useEffect, useMemo, useState } from 'react';
import { Typography } from 'antd';
import { FileOutlined } from '@ant-design/icons';
import FileViewer, {
  fileViewerFullPreset,
  setDefaultFullAssetBaseUrl,
  type ViewerOptions,
} from '@file-viewer/react-full';
import DownloadProgress from '@/app/components/preview/DownloadProgress';
import {
  fetchPreviewBytes,
  PreviewFetchAbortedError,
  type PreviewDownloadProgress,
} from '@/app/components/preview/fetchPreviewBytes';
import { getFileExtension } from '@/app/components/preview/previewKind';
import { useThemeStore } from '@/app/stores/themeStore';

const { Text } = Typography;

// Workers, WASM and fonts are staged by scripts/copy-file-viewer-assets.mjs
// into public/file-viewer, which Next's export ships at <origin>/file-viewer/.
// Pin the base explicitly: the library's inference reads script URLs, and
// Next's /_next/static/chunks layout is not one it recognises.
if (typeof window !== 'undefined') {
  setDefaultFullAssetBaseUrl(new URL('/file-viewer/', window.location.href).href);
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

interface FileViewerPaneProps {
  url: string;
  filename: string;
  /** CSS height of the viewer; the library needs an explicit one. */
  height: string;
}

export default function FileViewerPane({ url, filename, height }: FileViewerPaneProps) {
  const [file, setFile] = useState<File | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [progress, setProgress] = useState<PreviewDownloadProgress>({ loaded: 0, total: null });
  const appTheme = useThemeStore((s) => s.theme);
  const supported = isFileViewerSupported(filename);

  useEffect(() => {
    if (!supported) return;
    const controller = new AbortController();
    setFile(null);
    setError(null);
    setProgress({ loaded: 0, total: null });

    fetchPreviewBytes(url, { signal: controller.signal, onProgress: setProgress })
      .then((bytes) => {
        if (controller.signal.aborted) return;
        // A named File lets the viewer detect the format from the extension.
        setFile(new File([bytes as BlobPart], filename));
      })
      .catch((err: unknown) => {
        if (controller.signal.aborted || err instanceof PreviewFetchAbortedError) return;
        console.error('Failed to fetch preview:', err);
        setError(err instanceof Error ? err.message : 'Failed to load file');
      });

    return () => controller.abort();
  }, [url, filename, supported]);

  // The component reloads whenever this object's identity changes.
  const options = useMemo<ViewerOptions>(
    () => ({
      theme: appTheme === 'dark' ? 'dark' : 'light',
      toolbar: { position: 'bottom-right', download: false },
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
    <div style={{ height, textAlign: 'left' }} className="overflow-hidden rounded-lg">
      <FileViewer file={file} filename={filename} size={file.size} options={options} />
    </div>
  );
}
