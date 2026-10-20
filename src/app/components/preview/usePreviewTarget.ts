'use client';

import { useEffect, useState } from 'react';
import { fetchContentType } from '@/app/components/preview/fetchPreviewBytes';
import {
  getContentTypePreview,
  getFileExtension,
  getPreviewKind,
  type PreviewTarget,
} from '@/app/components/preview/previewKind';

export interface PreviewResolution {
  /** Where to preview the object; what its name calls for while `pending`. */
  target: PreviewTarget;
  /** The object's Content-Type is still being read. */
  pending: boolean;
  /** Why the Content-Type could not be read. */
  error: string | null;
}

interface ContentTypeLookup {
  filename: string;
  url: string;
  target?: PreviewTarget;
  error?: string;
}

function getNamePreview(filename: string, textExtensions: readonly string[]): PreviewTarget {
  return { kind: getPreviewKind(filename, textExtensions), extension: getFileExtension(filename) };
}

async function lookUpPreview(
  filename: string,
  url: string,
  textExtensions: readonly string[],
  signal: AbortSignal
): Promise<PreviewTarget> {
  const byName = getNamePreview(filename, textExtensions);
  // The pane that renders the object needs this module anyway.
  const { isFileViewerFormat } = await import('@/app/components/preview/fileViewerFormats');
  if (isFileViewerFormat(byName.extension)) return byName;
  const contentType = await fetchContentType(url, signal);
  return getContentTypePreview(contentType, textExtensions) ?? byName;
}

/**
 * Picks where to preview an object. Its name decides whenever a preview
 * handles the extension, and no request is made. Otherwise (no extension, as
 * with the UUID keys some backends store uploads under, or one no preview
 * renders) the object's Content-Type decides.
 */
export function usePreviewTarget(
  filename: string,
  size: number | undefined,
  url: string | null,
  textExtensions: readonly string[]
): PreviewResolution {
  const byName = getNamePreview(filename, textExtensions);
  // An empty object has nothing to preview, whatever its type.
  const lookupUrl = byName.kind === 'viewer' && size !== 0 ? url : null;
  const [lookup, setLookup] = useState<ContentTypeLookup | null>(null);

  useEffect(() => {
    if (!lookupUrl) return;
    const controller = new AbortController();
    lookUpPreview(filename, lookupUrl, textExtensions, controller.signal)
      .then((target) => {
        if (!controller.signal.aborted) setLookup({ filename, url: lookupUrl, target });
      })
      .catch((err: unknown) => {
        if (controller.signal.aborted) return;
        console.warn('Failed to read Content-Type for preview:', err);
        const error = err instanceof Error ? err.message : String(err);
        setLookup({ filename, url: lookupUrl, error });
      });
    return () => controller.abort();
  }, [filename, lookupUrl, textExtensions]);

  if (!lookupUrl) return { target: byName, pending: false, error: null };
  // Until the effect settles, a stored lookup belongs to the previous object.
  if (lookup?.filename !== filename || lookup.url !== lookupUrl) {
    return { target: byName, pending: true, error: null };
  }
  return { target: lookup.target ?? byName, pending: false, error: lookup.error ?? null };
}
