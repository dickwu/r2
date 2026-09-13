/** Actual QueryClient.setQueryData cost with production lexical-page snapshots.
 * Headless: excludes S3/SQLite/native IPC/React rendering and does not modify the app.
 */
import { readFileSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { QueryClient } from '@tanstack/react-query';
import { createFolderPageAccumulator, type FolderSnapshot } from '../src/app/utils/folderItems';

const output = process.argv[2] ?? 'docs/engineering/r2-audit/native-listing-query-sharing.json';
const count = 100_000;
const files = Array.from({ length: count }, (_, index) => `fixture/file-${index}.txt`)
  .sort()
  .map((key, index) => ({
    key,
    name: key,
    size: index + 1,
    last_modified: '2026-09-12T00:00:00Z',
  }));
const runs: Array<{
  trial: number;
  sharing: 'default' | 'disabled';
  accumulation_ms: number;
  set_query_data_ms: number;
  total_ms: number;
  final_count: number;
  final_snapshot_identity_preserved: boolean;
}> = [];
for (let trial = 0; trial < 10; trial++) {
  // Alternate order to avoid assigning all warm-up/GC behavior to one variant.
  for (const sharing of trial % 2 === 0
    ? (['default', 'disabled'] as const)
    : (['disabled', 'default'] as const)) {
    const client = new QueryClient({
      defaultOptions: {
        queries: {
          gcTime: Infinity,
          retry: false,
          ...(sharing === 'disabled' ? { structuralSharing: false } : {}),
        },
      },
    });
    const scope = {
      provider: 'fixture',
      account_id: 'fixture',
      bucket: 'fixture',
      prefix: 'fixture/',
      request_id: `${trial}-${sharing}`,
      generation: trial,
    };
    const key = ['folder-contents', scope.provider, scope.account_id, scope.bucket, scope.prefix];
    const accumulator = createFolderPageAccumulator(scope);
    let accumulationMs = 0;
    let queryMs = 0;
    let finalSnapshot: FolderSnapshot | null = null;
    const started = performance.now();
    for (let offset = 0; offset < count; offset += 1000) {
      const complete = offset + 1000 === count;
      const accumulateStart = performance.now();
      const snapshot = accumulator.accept({
        ...scope,
        files: files.slice(offset, offset + 1000),
        folders: [],
        page_index: offset / 1000,
        complete,
        next_cursor: complete ? null : String(offset + 1000),
        from_cache: false,
        freshness: 'fresh',
      });
      accumulationMs += performance.now() - accumulateStart;
      const queryStart = performance.now();
      client.setQueryData(key, snapshot);
      queryMs += performance.now() - queryStart;
      finalSnapshot = snapshot;
    }
    const totalMs = performance.now() - started;
    const stored = client.getQueryData<FolderSnapshot | null>(key);
    if (!stored?.complete || stored.items.length !== count)
      throw new Error('Incomplete QueryClient snapshot');
    if (sharing === 'disabled' && stored !== finalSnapshot)
      throw new Error('structuralSharing:false did not preserve the immutable snapshot');
    runs.push({
      trial,
      sharing,
      accumulation_ms: accumulationMs,
      set_query_data_ms: queryMs,
      total_ms: totalMs,
      final_count: stored.items.length,
      final_snapshot_identity_preserved: stored === finalSnapshot,
    });
    client.clear();
  }
}
const quantile = (values: number[], p: number) =>
  [...values].sort((a, b) => a - b)[Math.ceil(values.length * p) - 1];
const summaries = (['default', 'disabled'] as const).map((sharing) => {
  const samples = runs.filter((run) => run.sharing === sharing);
  return {
    sharing,
    trials: samples.length,
    set_query_data_ms: {
      p50: quantile(
        samples.map((sample) => sample.set_query_data_ms),
        0.5
      ),
      p95: quantile(
        samples.map((sample) => sample.set_query_data_ms),
        0.95
      ),
    },
    accumulation_ms: {
      p50: quantile(
        samples.map((sample) => sample.accumulation_ms),
        0.5
      ),
      p95: quantile(
        samples.map((sample) => sample.accumulation_ms),
        0.95
      ),
    },
    total_ms: {
      p50: quantile(
        samples.map((sample) => sample.total_ms),
        0.5
      ),
      p95: quantile(
        samples.map((sample) => sample.total_ms),
        0.95
      ),
    },
  };
});
const evidence = {
  scope:
    'Actual QueryClient.setQueryData on 100 lexical S3 pages through production immutable incremental accumulator; headless, no native/backend/React rendering',
  captured_at: new Date().toISOString(),
  items: count,
  page_size: 1000,
  order: 'alternating variants per trial',
  query_version: JSON.parse(
    readFileSync(
      new URL('../node_modules/@tanstack/react-query/package.json', import.meta.url),
      'utf8'
    )
  ).version,
  accumulator_sha256: createHash('sha256')
    .update(readFileSync(new URL('../src/app/utils/folderItems.ts', import.meta.url)))
    .digest('hex'),
  summaries,
  runs,
};
writeFileSync(output, JSON.stringify(evidence, null, 2) + '\n');
console.log(JSON.stringify(evidence, null, 2));
