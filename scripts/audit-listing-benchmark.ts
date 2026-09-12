/** Deterministic local benchmark of production page merging and cache handoff.
 * This excludes S3, SQLite, native IPC, React rendering and OS mount latency.
 * Run: bun scripts/audit-listing-benchmark.ts
 */
import { performance } from 'node:perf_hooks';
import {
  buildFileItems,
  createFolderPageAccumulator,
  loadFolderItems,
} from '../src/app/utils/folderItems';

const quantile = (values: number[], fraction: number) =>
  [...values].sort((a, b) => a - b)[
    Math.min(values.length - 1, Math.ceil(values.length * fraction) - 1)
  ];

const results = [];
for (const total of [10_000, 100_000]) {
  const firstPages: number[] = [];
  const completePages: number[] = [];
  const cacheHandoffs: number[] = [];
  const scope = {
    provider: 'fixture',
    account_id: 'fixture',
    bucket: 'fixture',
    prefix: '',
    request_id: 'benchmark',
    generation: 1,
  };
  const files = Array.from({ length: total }, (_, index) => ({
    key: `file-${index}.txt`,
    name: `file-${index}.txt`,
    size: index,
    last_modified: '2026-09-12T00:00:00Z',
  }));
  const cached = {
    items: buildFileItems(
      files.map((file) => ({ ...file, lastModified: file.last_modified })),
      [],
      ''
    ),
    complete: true,
    fromCache: true,
    freshness: 'stale' as const,
  };
  for (let run = 0; run < 10; run += 1) {
    const accumulator = createFolderPageAccumulator(scope);
    const start = performance.now();
    for (let index = 0; index < total / 1000; index += 1) {
      const complete = (index + 1) * 1000 === total;
      const snapshot = accumulator.accept({
        ...scope,
        page_index: index,
        files: files.slice(index * 1000, (index + 1) * 1000),
        folders: [],
        from_cache: false,
        complete,
        next_cursor: complete ? null : String(index + 1),
        freshness: complete ? 'fresh' : 'partial',
      });
      if (index === 0) firstPages.push(performance.now() - start);
      if (complete && snapshot?.items.length !== total)
        throw new Error('Incomplete benchmark listing');
    }
    completePages.push(performance.now() - start);
    const cachedStart = performance.now();
    let delivered = false;
    await loadFolderItems({
      config: {},
      prefix: '',
      readCachedFolder: async () => cached,
      readPrefixFolder: async () => {
        if (!delivered) throw new Error('Network refresh started before cache was delivered');
        return cached;
      },
      onUpdate: () => {
        delivered = true;
        cacheHandoffs.push(performance.now() - cachedStart);
      },
    });
  }
  results.push({
    total,
    page_size: 1000,
    runs: 10,
    first_page_ms: { p50: quantile(firstPages, 0.5), p95: quantile(firstPages, 0.95) },
    all_pages_ms: { p50: quantile(completePages, 0.5), p95: quantile(completePages, 0.95) },
    cached_snapshot_handoff_ms: {
      p50: quantile(cacheHandoffs, 0.5),
      p95: quantile(cacheHandoffs, 0.95),
    },
  });
}
process.stdout.write(
  `${JSON.stringify({ scope: 'In-process production merge/cache helpers; excludes backend and rendering', captured_at: new Date().toISOString(), results }, null, 2)}\n`
);
