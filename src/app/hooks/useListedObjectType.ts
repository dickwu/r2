import { useQuery } from '@tanstack/react-query';
import type { FileItem } from '@/app/hooks/useR2Files';
import { usePreviewUrl } from '@/app/hooks/usePreviewUrl';
import { detectObjectType } from '@/app/components/preview/fetchPreviewBytes';
import {
  getDetectedPreview,
  getFileExtension,
  type PreviewTarget,
} from '@/app/components/preview/previewKind';
import type { StorageConfig } from '@/app/lib/r2cache';

/**
 * What an extension-less object is, going by its content (sniffed by the
 * backend), for the views that list objects (grid thumbnails, inspector).
 * Null until it is known, and always for names with an extension, which keep
 * deciding on their own without a request.
 */
export function useListedObjectType(
  config: StorageConfig | null | undefined,
  item: FileItem
): PreviewTarget | null {
  const enabled = !item.isFolder && item.size !== 0 && !getFileExtension(item.name);
  const url = usePreviewUrl(config, item.key, enabled);
  const { data: detected } = useQuery({
    queryKey: ['object-type', config?.provider, config?.accountId, config?.bucket, item.key],
    queryFn: () => detectObjectType(url!),
    enabled: enabled && !!url,
    // What an object is only changes when it is overwritten.
    staleTime: Infinity,
    retry: false,
  });
  // Lists only classify: neither the editor nor the viewer has to render it.
  return enabled && detected ? getDetectedPreview(detected, [], () => true) : null;
}
