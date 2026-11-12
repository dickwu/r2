import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

describe('file listing loading spinner', () => {
  const source = readFileSync(fileURLToPath(new URL('page.tsx', import.meta.url)), 'utf8');

  test('does not use a fullscreen overlay that blocks sidebar bucket clicks', () => {
    const match = source.match(/className="file-list-loading"[\s\S]{0,400}?<Spin\b[^>]*\/?>/);
    expect(match).not.toBeNull();
    expect(match![0]).not.toContain('fullscreen');
  });
});
