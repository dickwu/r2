import { describe, expect, test } from 'bun:test';
import {
  compareVersions,
  entriesToMarkdown,
  fetchReleaseFeed,
  formatReleaseDate,
  githubReleaseUrl,
  parseReleaseFeed,
  releaseBody,
  releasesBetween,
  releasesUpTo,
  type FetchLike,
} from './releaseNotes';

const raw = {
  releases: [
    {
      version: '0.3.18',
      date: null,
      draft: true,
      title: 'Updates show what changed',
      summary: 'Summary 18.',
      notes: { new: ['Update notes.'], fixed: ['A fix with `code`.'] },
    },
    {
      version: '0.3.17',
      date: '2026-10-26',
      title: 'Edit files',
      summary: 'Summary 17.',
      notes: { new: ['Editor.'] },
    },
    { version: '0.3.16', date: '2026-10-23', listed: false, title: 'Deps', summary: 'Summary 16.' },
    { version: 'nope', title: 'bad' },
    {
      version: '0.3.15',
      date: '2026-10-23',
      title: 'Queue',
      summary: 'Summary 15.',
      notes: { improved: ['Retries.', 3] },
    },
  ],
};

const feed = parseReleaseFeed(raw)!;

describe('compareVersions', () => {
  test('orders numerically, not as text', () => {
    expect(compareVersions('0.3.10', '0.3.9')).toBeGreaterThan(0);
    expect(compareVersions('v0.3.17', '0.3.17')).toBe(0);
    expect(compareVersions('0.2.99', '0.3.0')).toBeLessThan(0);
    expect(compareVersions('1.0.0', '0.99.99')).toBeGreaterThan(0);
  });
});

describe('parseReleaseFeed', () => {
  test('keeps valid entries newest first and drops the rest', () => {
    expect(feed.releases.map((r) => r.version)).toEqual(['0.3.18', '0.3.17', '0.3.16', '0.3.15']);
    expect(feed.latest).toBe('0.3.17');
    expect(feed.releases[0].draft).toBe(true);
    expect(feed.releases[3].notes).toEqual({ new: [], improved: ['Retries.'], fixed: [] });
    expect(feed.releases[1].url).toBe('https://github.com/dickwu/r2/releases/tag/v0.3.17');
  });

  test('rejects anything that is not a feed', () => {
    expect(parseReleaseFeed(null)).toBeNull();
    expect(parseReleaseFeed({ releases: 'x' })).toBeNull();
    expect(parseReleaseFeed({ releases: [{ version: 'bad' }] })).toBeNull();
  });
});

describe('ranges', () => {
  test('an update covers every version after yours up to the new one', () => {
    expect(releasesBetween(feed, '0.3.15', '0.3.18').map((r) => r.version)).toEqual([
      '0.3.18',
      '0.3.17',
      '0.3.16',
    ]);
    expect(releasesBetween(feed, '0.3.17', '0.3.17')).toEqual([]);
  });

  test('Settings lists up to the running version; drafts only once you run them', () => {
    expect(releasesUpTo(feed, '0.3.17').map((r) => r.version)).toEqual([
      '0.3.17',
      '0.3.16',
      '0.3.15',
    ]);
    expect(releasesUpTo(feed, '0.3.18')[0].version).toBe('0.3.18');
    expect(releasesUpTo(feed, null).some((r) => r.draft)).toBe(false);
  });
});

describe('entriesToMarkdown', () => {
  test('one version: sections only, the summary only when there are no notes', () => {
    expect(entriesToMarkdown([feed.releases[0]])).toBe(
      '## New\n\n- Update notes.\n\n## Fixed\n\n- A fix with `code`.'
    );
    expect(entriesToMarkdown([feed.releases[2]])).toBe('Summary 16.');
  });

  test('a release body leads with the summary', () => {
    expect(entriesToMarkdown([feed.releases[1]], { includeSummary: true })).toBe(
      'Summary 17.\n\n## New\n\n- Editor.'
    );
  });

  test('several versions get a heading each', () => {
    const md = entriesToMarkdown(releasesBetween(feed, '0.3.16', '0.3.18'));
    expect(md).toBe(
      '## v0.3.18: Updates show what changed\n\n### New\n\n- Update notes.\n\n### Fixed\n\n- A fix with `code`.\n\n## v0.3.17: Edit files\n\n### New\n\n- Editor.'
    );
  });
});

describe('formatting', () => {
  test('dates read as calendar dates', () => {
    expect(formatReleaseDate('2026-10-26')).toBe('October 26, 2026');
    expect(formatReleaseDate('2026-01-01')).toBe('January 1, 2026');
    expect(formatReleaseDate(null)).toBe('');
  });
  test('GitHub release links', () => {
    expect(githubReleaseUrl('v0.3.18')).toBe('https://github.com/dickwu/r2/releases/tag/v0.3.18');
  });
});

describe('fetchReleaseFeed', () => {
  const ok =
    (body: unknown): FetchLike =>
    async () => ({ ok: true, status: 200, json: async () => body });

  test('reads a feed', async () => {
    expect((await fetchReleaseFeed(ok(raw)))?.latest).toBe('0.3.17');
  });

  test('null on HTTP errors, bad JSON, network errors, and timeouts', async () => {
    expect(
      await fetchReleaseFeed(async () => ({ ok: false, status: 404, json: async () => raw }))
    ).toBeNull();
    expect(await fetchReleaseFeed(ok({ nope: true }))).toBeNull();
    expect(
      await fetchReleaseFeed(async () => {
        throw new Error('offline');
      })
    ).toBeNull();
    const hang: FetchLike = (_url, init) =>
      new Promise((_resolve, reject) =>
        init?.signal?.addEventListener('abort', () => reject(new Error('aborted')))
      );
    expect(await fetchReleaseFeed(hang, 'https://x', 20)).toBeNull();
  });
});

describe('releaseBody', () => {
  test('summary, notes, and a compare link to the version before', () => {
    expect(releaseBody(feed, 'v0.3.17')).toBe(
      'Summary 17.\n\n## New\n\n- Editor.\n\n**Full Changelog**: https://github.com/dickwu/r2/compare/v0.3.16...v0.3.17'
    );
    expect(releaseBody(feed, '0.3.15')).toBe('Summary 15.\n\n## Improved\n\n- Retries.');
    expect(releaseBody(feed, '0.9.0')).toBeNull();
  });
});
