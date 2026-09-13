/** Production accumulator with lexical S3 page order; excludes native/backend/rendering. */
import { readFileSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { createFolderPageAccumulator } from '../src/app/utils/folderItems';

const output = process.argv[2];
if (!output) throw new Error('Pass a native-listing*.json output path');
const results = [];
for (const count of [10_000, 100_000]) {
  const keys = Array.from({ length: count }, (_, index) => `fixture/file-${index}.txt`).sort();
  const files = keys.map((key, index) => ({
    key,
    name: key,
    size: index + 1,
    last_modified: '2026-09-12T00:00:00Z',
  }));
  const elapsed = [];
  for (let trial = 0; trial < 10; trial++) {
    const scope = {
      provider: 'fixture',
      account_id: 'fixture',
      bucket: 'fixture',
      prefix: 'fixture/',
      request_id: String(trial),
      generation: trial,
    };
    const accumulator = createFolderPageAccumulator(scope);
    const start = performance.now();
    for (let offset = 0; offset < count; offset += 1000) {
      const complete = offset + 1000 === count;
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
      if (complete && snapshot?.items.length !== count) throw new Error('Incomplete listing');
    }
    elapsed.push(performance.now() - start);
  }
  const sorted = [...elapsed].sort((a, b) => a - b);
  results.push({
    items: count,
    pages: count / 1000,
    trials: elapsed.length,
    raw_ms: elapsed,
    p50_ms: sorted[4],
    p95_ms: sorted[9],
  });
}
const evidence = {
  scope:
    'In-process production accumulation with natural sorting of lexical S3 pages; excludes SDK, SQLite, IPC and rendering',
  captured_at: new Date().toISOString(),
  source_sha256: createHash('sha256')
    .update(readFileSync(new URL('../src/app/utils/folderItems.ts', import.meta.url)))
    .digest('hex'),
  results,
};
writeFileSync(output, JSON.stringify(evidence, null, 2) + '\n');
console.log(JSON.stringify(evidence, null, 2));
