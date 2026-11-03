'use client';

import { useEffect, useRef } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import {
  buildPublicUrl,
  generateSignedUrl,
  hasSigningCredentials,
  isBucketPublic,
  type StorageConfig,
} from '@/app/lib/r2cache';
import {
  planThumbnailRefresh,
  THUMBNAIL_CLEARED_EVENT,
  THUMBNAIL_INVALIDATED_EVENT,
  type ThumbnailInvalidation,
} from '@/app/lib/thumbnailIdentity';
import { clearThumbnailMemory, forgetThumbnails, getThumbnailSrc } from '@/app/hooks/useThumbnail';

/** Changed objects remade at a time; the native side also limits image decodes. */
const REFRESH_CONCURRENCY = 2;

async function sourceUrl(config: StorageConfig, key: string): Promise<string | null> {
  if (isBucketPublic(config)) return buildPublicUrl(config, key);
  if (hasSigningCredentials(config)) return generateSignedUrl(config, key, 15 * 60);
  return null;
}

/**
 * Keeps the session thumbnails in step with the native cache. A listing or
 * sync that finds an object changed or gone makes the native side drop its
 * thumbnail and say so; here the old version is forgotten and, for the bucket
 * on screen, the changed ones are remade right away so the grid shows the new
 * picture without waiting for a miss.
 */
export function useThumbnailCacheSync(config: StorageConfig | null | undefined) {
  const configRef = useRef(config);
  configRef.current = config;

  useEffect(() => {
    let disposed = false;
    const unlisteners: UnlistenFn[] = [];
    const add = async <T>(name: string, receive: (payload: T) => void) => {
      try {
        const unlisten = await listen<T>(name, (event) => receive(event.payload));
        if (disposed) unlisten();
        else unlisteners.push(unlisten);
      } catch {
        // Outside Tauri (tests, plain browser) there are no native events.
      }
    };

    void add<ThumbnailInvalidation>(THUMBNAIL_INVALIDATED_EVENT, (event) => {
      if (event.accountId && event.bucket) {
        forgetThumbnails(event.accountId, event.bucket, [
          ...(event.changed ?? []).map((item) => item.key),
          ...(event.removed ?? []),
        ]);
      }
      const current = configRef.current;
      const plan = planThumbnailRefresh(event, current);
      if (!current || plan.regenerate.length === 0) return;
      const queue = [...plan.regenerate];
      const worker = async () => {
        for (let next = queue.shift(); next && !disposed; next = queue.shift()) {
          const { kind, ...object } = next;
          try {
            const url = await sourceUrl(current, object.key);
            if (url) await getThumbnailSrc(kind, url, object);
          } catch {
            // A tile that needs it will try again.
          }
        }
      };
      for (let i = 0; i < REFRESH_CONCURRENCY; i += 1) void worker();
    });
    void add<null>(THUMBNAIL_CLEARED_EVENT, () => clearThumbnailMemory());

    return () => {
      disposed = true;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, []);
}
