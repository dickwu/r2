import { useMemo } from 'react';
import { useUploadStore } from '@/app/stores/uploadStore';
import { useDownloadStore } from '@/app/stores/downloadStore';
import { useMoveStore } from '@/app/stores/moveStore';
import { useMountStore } from '@/app/stores/mountStore';
import { selectUnsavedEditorCount, useEditorDirtyStore } from '@/app/stores/editorDirtyStore';
import { countActiveTransfers, type ActiveWork } from '@/app/lib/updates/activeWork';

/** Work a restart would interrupt right now. */
export function useActiveWork(): ActiveWork {
  const uploads = useUploadStore((s) => s.tasks);
  const downloads = useDownloadStore((s) => s.tasks);
  const moves = useMoveStore((s) => s.tasks);
  const mounts = useMountStore((s) => s.mounts);
  const unsavedEditors = useEditorDirtyStore(selectUnsavedEditorCount);

  return useMemo(
    () => ({
      transfers: countActiveTransfers(uploads, downloads, moves),
      mountUploads: mounts.reduce((sum, mount) => sum + (mount.pendingUploads ?? 0), 0),
      unsavedEditors,
    }),
    [uploads, downloads, moves, mounts, unsavedEditors]
  );
}
