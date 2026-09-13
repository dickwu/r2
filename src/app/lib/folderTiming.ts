import { redactSecrets } from './diagnostics/redact';

type FolderCommand = 'get_prefix_cache' | 'list_prefix_stream' | 'cancel_prefix_list';
interface FolderScope {
  provider: string;
  account_id: string;
  bucket: string;
  prefix: string;
  request_id?: string;
  generation?: number;
}

let sequence = 0;

function resultSummary(value: unknown): Record<string, unknown> | undefined {
  if (!value || typeof value !== 'object') return undefined;
  const result = value as Record<string, unknown>;
  const summary: Record<string, unknown> = {};
  for (const key of ['complete', 'from_cache', 'freshness', 'total_items']) {
    if (['string', 'number', 'boolean'].includes(typeof result[key])) summary[key] = result[key];
  }
  for (const key of ['files', 'folders']) {
    if (Array.isArray(result[key])) summary[key] = result[key].length;
  }
  if (result.timing && typeof result.timing === 'object') {
    summary.timing = Object.fromEntries(
      Object.entries(result.timing).filter(
        ([, value]) => typeof value === 'number' || typeof value === 'boolean'
      )
    );
  }
  return summary;
}

/** Optional observation for the isolated native acceptance harness.
 * No request arguments, credentials or object arrays enter the observation.
 * The normal app takes the original path without timing/event overhead.
 */
export async function observeFolderRequest<T>(
  command: FolderCommand,
  scope: FolderScope,
  send: () => Promise<T>
): Promise<T> {
  if (
    typeof window === 'undefined' ||
    !(window as Window & { __nativeListingAudit?: unknown }).__nativeListingAudit
  ) {
    return send();
  }
  const operation_id = `folder-${++sequence}`;
  const observedScope = {
    provider: scope.provider,
    account_id: scope.account_id,
    bucket: scope.bucket,
    prefix: scope.prefix,
    request_id: scope.request_id,
    generation: scope.generation,
  };
  const emit = (phase: 'start' | 'complete' | 'error', extra = {}) => {
    try {
      window.dispatchEvent(
        new CustomEvent('r2-listing-observation', {
          detail: {
            ...observedScope,
            operation_id,
            command,
            phase,
            at: performance.now(),
            ...extra,
          },
        })
      );
    } catch {
      // Diagnostics must never change an operation's outcome.
    }
  };
  emit('start');
  try {
    const result = await send();
    emit('complete', { result: resultSummary(result) });
    return result;
  } catch (error) {
    emit('error', { error: redactSecrets(String(error)).slice(0, 1000) });
    throw error;
  }
}

/** Timing for the complete production page merge, including its delta sort.
 * The optional harness also measures sort calls; these are nested, not additive.
 */
export function observeFolderAccumulation<T>(
  scope: FolderScope,
  pageIndex: number,
  accumulate: () => T
): T {
  if (
    typeof window === 'undefined' ||
    !(window as Window & { __nativeListingAudit?: unknown }).__nativeListingAudit
  ) {
    return accumulate();
  }
  const startedAt = performance.now();
  try {
    return accumulate();
  } finally {
    const finishedAt = performance.now();
    try {
      window.dispatchEvent(
        new CustomEvent('r2-listing-accumulation', {
          detail: {
            provider: scope.provider,
            account_id: scope.account_id,
            bucket: scope.bucket,
            prefix: scope.prefix,
            request_id: scope.request_id,
            generation: scope.generation,
            page_index: pageIndex,
            started_at: startedAt,
            finished_at: finishedAt,
            duration_ms: finishedAt - startedAt,
          },
        })
      );
    } catch {
      // Observation cannot change the merge result or its original error.
    }
  }
}
