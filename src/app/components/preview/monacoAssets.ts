import { loader } from '@monaco-editor/react';

/** Where scripts/copy-monaco-assets.mjs stages Monaco's AMD build, relative to the app origin. */
export const MONACO_VS_PATH = '/monaco-editor/vs';

/**
 * Points @monaco-editor/react's loader at the Monaco build staged with the app.
 *
 * Left unconfigured, the loader injects its own default Monaco from jsdelivr:
 * the editor then never loads offline, and it runs a different version than
 * the monaco-editor package the app pins and compiles its types against.
 */
export function configureMonacoLoader(documentUrl: string): string {
  const vs = new URL(MONACO_VS_PATH, documentUrl).href;
  loader.config({ paths: { vs } });
  return vs;
}
