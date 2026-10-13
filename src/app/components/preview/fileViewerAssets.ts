import { setDefaultFileViewerAssetBaseUrl } from '@file-viewer/core/assets';
import { setDefaultFullAssetBaseUrl } from '@file-viewer/preset-all';

/** Where scripts/copy-file-viewer-assets.mjs stages the payload, relative to the app origin. */
export const FILE_VIEWER_ASSET_PATH = '/file-viewer/';

/**
 * Points every file-viewer renderer at the staged Worker/WASM/font payload.
 *
 * Two setters are needed: the full preset only fills the URL options of the
 * renderers it knows about (PDF, Office, CAD, archives, …), while iWork,
 * Hangul and WordPerfect resolve their Workers through core's own default,
 * which would otherwise fall back to the document base and 404.
 */
export function configureFileViewerAssetBase(documentUrl: string): string {
  const base = new URL(FILE_VIEWER_ASSET_PATH, documentUrl).href;
  setDefaultFileViewerAssetBaseUrl(base);
  setDefaultFullAssetBaseUrl(base);
  return base;
}
