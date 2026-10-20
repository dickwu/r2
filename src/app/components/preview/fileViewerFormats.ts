import { fileViewerFullPreset } from '@file-viewer/react-full';

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

export function isFileViewerFormat(extension: string): boolean {
  return SUPPORTED_EXTENSIONS.has(extension);
}
