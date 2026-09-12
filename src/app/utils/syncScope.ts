export interface SyncScope {
  provider: string;
  account_id: string;
  bucket: string;
  prefix: string;
  run_id: string;
}

export function matchesSyncRun(active: SyncScope | null, event: SyncScope): boolean {
  return (
    active !== null &&
    active.provider === event.provider &&
    active.account_id === event.account_id &&
    active.bucket === event.bucket &&
    active.prefix === event.prefix &&
    active.run_id === event.run_id
  );
}

interface MoveScope {
  sourceProvider: string;
  sourceAccountId: string;
  sourceBucket: string;
  sourceKey: string;
  destProvider: string;
  destAccountId: string;
  destBucket: string;
  destKey: string;
  deleteOriginal: boolean;
}

export function parentPrefix(path: string): string {
  const withoutTrailing = path.replace(/\/$/, '');
  const slash = withoutTrailing.lastIndexOf('/');
  return slash < 0 ? '' : withoutTrailing.slice(0, slash + 1);
}

/** Refresh only directory queries touched by this move, including ancestor metadata. */
export function affectedMoveFolders(task: MoveScope): string[][] {
  const result = new Map<string, string[]>();
  const add = (provider: string, account: string, bucket: string, key: string) => {
    let prefix = parentPrefix(key);
    for (;;) {
      const queryKey = ['folder-contents', provider, account, bucket, prefix];
      result.set(JSON.stringify(queryKey), queryKey);
      if (!prefix) break;
      prefix = parentPrefix(prefix);
    }
  };
  if (task.deleteOriginal)
    add(task.sourceProvider, task.sourceAccountId, task.sourceBucket, task.sourceKey);
  add(task.destProvider, task.destAccountId, task.destBucket, task.destKey);
  return Array.from(result.values());
}

/** One refresh per affected query in a burst; a maximum delay prevents starvation. */
export function createFolderInvalidator(invalidate: (queryKey: string[]) => void, delayMs = 250) {
  const pending = new Map<string, string[]>();
  let timer: ReturnType<typeof setTimeout> | undefined;
  return {
    add(queryKeys: string[][]) {
      for (const queryKey of queryKeys) pending.set(JSON.stringify(queryKey), queryKey);
      if (timer) return;
      timer = setTimeout(() => {
        timer = undefined;
        const keys = Array.from(pending.values());
        pending.clear();
        keys.forEach(invalidate);
      }, delayMs);
    },
    dispose() {
      if (timer) clearTimeout(timer);
      timer = undefined;
      pending.clear();
    },
  };
}
