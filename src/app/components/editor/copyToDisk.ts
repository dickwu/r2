import { save } from '@tauri-apps/plugin-dialog';
import { writeFile } from '@tauri-apps/plugin-fs';
import type { CopyDependencies } from './editedContent';

/** The native save dialog and file write behind "Download edited copy". */
export const tauriCopyDependencies: CopyDependencies = {
  pickPath: async (defaultPath, filters) =>
    (await save({ defaultPath, filters: filters.length ? filters : undefined })) ?? null,
  writeFile: (path, bytes) => writeFile(path, bytes),
};
