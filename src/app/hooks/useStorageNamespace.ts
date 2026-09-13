import { useEffect, useState } from 'react';
import type { StorageConfig } from '@/app/lib/r2cache';
import { hashStorageNamespace, storageNamespaceInput } from '@/app/utils/storageNamespace';

/** A changed config immediately loses access to old rows while its hash resolves. */
export function useStorageNamespace(config: StorageConfig | null): string | null {
  const input = storageNamespaceInput(config);
  const [resolved, setResolved] = useState<{ input: string; hash: string } | null>(null);
  useEffect(() => {
    if (!input) return;
    let disposed = false;
    void hashStorageNamespace(input).then((hash) => {
      if (!disposed) setResolved({ input, hash });
    });
    return () => {
      disposed = true;
    };
  }, [input]);
  return resolved?.input === input ? resolved.hash : null;
}
