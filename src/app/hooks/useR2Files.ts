import { useQuery, useQueryClient } from '@tanstack/react-query';
import { listAllR2Objects, R2Object } from '../lib/r2cache';
import { R2Config } from '../components/ConfigModal';

export interface FileItem {
  name: string;
  key: string;
  isFolder: boolean;
  size?: number;
  lastModified?: string;
}

function extractName(key: string, prefix: string): string {
  const relativePath = prefix ? key.slice(prefix.length) : key;
  return relativePath.replace(/\/$/, '');
}

function buildFileItems(objects: R2Object[], folders: string[], prefix: string): FileItem[] {
  const items: FileItem[] = [];

  // Add folders first
  for (const folder of folders) {
    items.push({
      name: extractName(folder, prefix),
      key: folder,
      isFolder: true,
    });
  }

  // Add files
  for (const obj of objects) {
    if (obj.key === prefix || obj.key.endsWith('/')) continue;
    items.push({
      name: extractName(obj.key, prefix),
      key: obj.key,
      isFolder: false,
      size: obj.size,
      lastModified: obj.last_modified,
    });
  }

  return items.sort((a, b) => {
    if (a.isFolder && !b.isFolder) return -1;
    if (!a.isFolder && b.isFolder) return 1;
    return a.name.localeCompare(b.name);
  });
}

export function useR2Files(config: R2Config | null, prefix: string = '') {
  const queryClient = useQueryClient();
  const queryKey = ['r2-files', config?.bucket, prefix];

  const query = useQuery({
    queryKey,
    queryFn: async (): Promise<FileItem[]> => {
      if (!config) return [];
      console.log('Fetching R2 files:', { bucket: config.bucket, prefix });

      const result = await listAllR2Objects(config, prefix);
      console.log('R2 API result:', {
        objects: result.objects.length,
        folders: result.folders.length,
      });

      return buildFileItems(result.objects, result.folders, prefix);
    },
    enabled: !!config?.token && !!config?.bucket && !!config?.accountId,
    retry: 1,
  });

  async function refresh() {
    await queryClient.invalidateQueries({ queryKey });
  }

  return {
    items: query.data ?? [],
    isLoading: query.isLoading,
    isFetching: query.isFetching,
    error: query.error,
    refresh,
  };
}
