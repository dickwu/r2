import { useQuery } from '@tanstack/react-query';
import type { FileItem } from '@/app/hooks/useR2Files';
import { usePreviewUrl } from '@/app/hooks/usePreviewUrl';
import { fetchContentType } from '@/app/components/preview/fetchPreviewBytes';
import {
  getContentTypePreview,
  getFileExtension,
  type PreviewTarget,
} from '@/app/components/preview/previewKind';
import type { StorageConfig } from '@/app/lib/r2cache';

/**
 * What an extension-less object is, going by its Content-Type, for the views
 * that list objects (grid thumbnails, inspector). Null until the type is
 * known, and always for names with an extension, which keep deciding on their
 * own without a request.
 */
export function useListedObjectType(
  config: StorageConfig | null | undefined,
  item: FileItem
): PreviewTarget | null {
  const enabled = !item.isFolder && item.size !== 0 && !getFileExtension(item.name);
  const url = usePreviewUrl(config, item.key, enabled);
  const { data: contentType } = useQuery({
    queryKey: ['content-type', config?.provider, config?.accountId, config?.bucket, item.key],
    queryFn: ({ signal }) => fetchContentType(url!, signal),
    enabled: enabled && !!url,
    // A Content-Type only changes when the object is overwritten.
    staleTime: Infinity,
    retry: false,
  });
  // Lists never open the editor, so text formats need no routing of their own.
  return enabled ? getContentTypePreview(contentType, []) : null;
}
