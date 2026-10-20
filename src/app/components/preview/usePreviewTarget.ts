'use client';

import { useEffect, useState } from 'react';
import { detectObjectType } from '@/app/components/preview/fetchPreviewBytes';
import {
  contradictsName,
  getContentTypePreview,
  getDetectedPreview,
  getExtensionPreview,
  getFileExtension,
  type PreviewTarget,
} from '@/app/components/preview/previewKind';

/** Whether a preview follows the object's name or its detected content. */
export type PreviewSource = 'name' | 'content';

export interface PreviewChoices {
  name: PreviewTarget;
  content: PreviewTarget;
  /** What the content was identified as, e.g. "Portable Network Graphics". */
  contentName: string;
}

export interface PreviewResolution {
  /** What to render. */
  target: PreviewTarget;
  /** Nothing can render until the content is identified. */
  pending: boolean;
  /** Why the content could not be identified, when the name can't stand in. */
  error: string | null;
  /** Set when the name and the content call for different renderings. */
  choices: PreviewChoices | null;
  source: PreviewSource;
  setSource: (source: PreviewSource) => void;
}

interface Lookup {
  objectKey: string;
  url: string;
  /** Whether the viewer renders the name's format; unknown until its module loads. */
  nameRenders?: boolean;
  content?: { target: PreviewTarget | null; name: string };
  error?: string;
}

const loadViewerFormats = () => import('@/app/components/preview/fileViewerFormats');

/** Whether the webview's player may play a media type ('' is a no). */
const canPlayType = (mimeType: string) =>
  document.createElement('video').canPlayType(mimeType) !== '';

/**
 * Picks where to preview an object. Its content is identified in the
 * background (the first bytes, sniffed by the backend) whatever its name:
 *
 * - a name whose format renders shows at once, as before; when the content
 *   contradicts it (`contradictsName`), both are offered and the name's stays
 *   selected until the user switches;
 * - otherwise (no extension, as with the UUID keys some backends store
 *   uploads under, or one nothing renders) the content decides.
 */
export function usePreviewTarget(
  file: { key: string; name: string; size?: number } | null,
  url: string | null,
  textExtensions: readonly string[]
): PreviewResolution {
  const objectKey = file?.key ?? '';
  const filename = file?.name ?? '';
  const byName = getExtensionPreview(getFileExtension(filename), textExtensions);
  // An empty object has nothing to identify.
  const lookupUrl = file?.size !== 0 ? url : null;
  const [lookup, setLookup] = useState<Lookup | null>(null);
  // The user's pick per object, kept while the preview stays open.
  const [sources, setSources] = useState<Readonly<Record<string, PreviewSource>>>({});

  useEffect(() => {
    if (!lookupUrl) return;
    let stale = false;
    const update = (patch: Partial<Lookup>) => {
      if (stale) return;
      setLookup((prev) =>
        prev?.objectKey === objectKey && prev.url === lookupUrl
          ? { ...prev, ...patch }
          : { objectKey, url: lookupUrl, ...patch }
      );
    };
    const name = getExtensionPreview(getFileExtension(filename), textExtensions);

    (async () => {
      try {
        const [detected] = await Promise.all([
          detectObjectType(lookupUrl),
          // Whether the name renders is known long before the content is.
          name.kind === 'viewer' &&
            loadViewerFormats().then(
              ({ isFileViewerFormat }) =>
                update({ nameRenders: isFileViewerFormat(name.extension) }),
              // Without the viewer nothing renders the name's format.
              () => update({ nameRenders: false })
            ),
        ]);
        // The viewer's format list is only needed when the content may call for it.
        const mayNeedViewer = [
          getExtensionPreview(detected.extension, textExtensions),
          getContentTypePreview(detected.contentType, textExtensions),
        ].some((target) => target?.kind === 'viewer');
        const isViewerFormat = mayNeedViewer
          ? (await loadViewerFormats()).isFileViewerFormat
          : () => false;
        const target = getDetectedPreview(detected, textExtensions, isViewerFormat, canPlayType);
        update({ content: { target, name: detected.name } });
      } catch (err) {
        console.warn('Failed to identify the object for preview:', err);
        update({ error: err instanceof Error ? err.message : String(err) });
      }
    })();

    return () => {
      stale = true;
    };
  }, [objectKey, filename, lookupUrl, textExtensions]);

  const current =
    lookupUrl && lookup?.objectKey === objectKey && lookup.url === lookupUrl ? lookup : null;
  const chosenSource = sources[objectKey] ?? 'name';
  const setSource = (source: PreviewSource) =>
    setSources((prev) => ({ ...prev, [objectKey]: source }));
  const resolution = (patch: Partial<PreviewResolution>): PreviewResolution => ({
    target: byName,
    pending: false,
    error: null,
    choices: null,
    source: 'name',
    setSource,
    ...patch,
  });

  if (!lookupUrl) return resolution({});
  const nameRenders = byName.kind !== 'viewer' || current?.nameRenders;
  if (nameRenders === undefined) return resolution({ pending: true });

  const content = current?.content?.target ?? null;
  if (nameRenders) {
    if (!content || !contradictsName(byName, content)) return resolution({});
    const choices = { name: byName, content, contentName: current?.content?.name ?? '' };
    return chosenSource === 'content'
      ? resolution({ target: content, choices, source: 'content' })
      : resolution({ choices });
  }

  // The name can't render it: the content decides.
  if (current?.error) return resolution({ error: current.error });
  if (!current?.content) return resolution({ pending: true });
  return resolution({ target: content ?? byName, source: content ? 'content' : 'name' });
}
