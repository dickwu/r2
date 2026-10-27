/**
 * Release notes come from one place: r2-home's `data/releases.json`, published
 * as https://r2.lifefarmer.ca/releases.json. The update dialog, the What's new
 * note, Settings' release notes, and `scripts/release-notes.ts` (which writes the
 * GitHub Release body and the `latest.json` notes) all read that feed, so a
 * version reads the same everywhere. Nothing here touches Tauri, so the
 * publish script and the tests import it directly.
 */

export const RELEASES_URL = 'https://r2.lifefarmer.ca/releases.json';
export const CHANGELOG_URL = 'https://r2.lifefarmer.ca/#changelog';
export const REPOSITORY_URL = 'https://github.com/dickwu/r2';
export const GITHUB_RELEASES_URL = `${REPOSITORY_URL}/releases`;

export const NOTE_GROUPS = ['new', 'improved', 'fixed'] as const;
export type NoteGroup = (typeof NOTE_GROUPS)[number];

export const NOTE_GROUP_LABELS: Record<NoteGroup, string> = {
  new: 'New',
  improved: 'Improved',
  fixed: 'Fixed',
};

export interface ReleaseEntry {
  version: string;
  /** `YYYY-MM-DD`; null while the version is a draft. */
  date: string | null;
  /** Written ahead of the release; the site does not show it yet. */
  draft: boolean;
  title: string;
  /** One paragraph, Markdown. */
  summary: string;
  notes: Record<NoteGroup, string[]>;
  url: string;
}

export interface ReleaseFeed {
  /** The newest version that is not a draft. */
  latest: string | null;
  /** Newest first. */
  releases: ReleaseEntry[];
}

const VERSION_RE = /^v?(\d+)\.(\d+)\.(\d+)(?:[-+].*)?$/;

export const normalizeVersion = (version: string): string => version.trim().replace(/^v/, '');

export const isVersion = (value: unknown): value is string =>
  typeof value === 'string' && VERSION_RE.test(value.trim());

/** Negative when `a` is older than `b`. Unparseable versions sort oldest. */
export const compareVersions = (a: string, b: string): number => {
  const pa = VERSION_RE.exec(a.trim());
  const pb = VERSION_RE.exec(b.trim());
  if (!pa || !pb) return pa ? 1 : pb ? -1 : 0;
  for (let i = 1; i <= 3; i++) {
    const diff = Number(pa[i]) - Number(pb[i]);
    if (diff !== 0) return diff;
  }
  return 0;
};

export const githubReleaseUrl = (version: string): string =>
  `${GITHUB_RELEASES_URL}/tag/v${normalizeVersion(version)}`;

const stringList = (value: unknown): string[] =>
  Array.isArray(value)
    ? value.filter((item): item is string => typeof item === 'string' && item.trim() !== '')
    : [];

const parseEntry = (raw: unknown): ReleaseEntry | null => {
  if (!raw || typeof raw !== 'object') return null;
  const entry = raw as Record<string, unknown>;
  if (!isVersion(entry.version)) return null;
  const version = normalizeVersion(entry.version);
  const notes = (entry.notes && typeof entry.notes === 'object' ? entry.notes : {}) as Record<
    string,
    unknown
  >;
  return {
    version,
    date:
      typeof entry.date === 'string' && /^\d{4}-\d{2}-\d{2}$/.test(entry.date) ? entry.date : null,
    draft: entry.draft === true,
    title: typeof entry.title === 'string' ? entry.title : '',
    summary: typeof entry.summary === 'string' ? entry.summary : '',
    notes: {
      new: stringList(notes.new),
      improved: stringList(notes.improved),
      fixed: stringList(notes.fixed),
    },
    url:
      typeof entry.url === 'string' && /^https:\/\//.test(entry.url)
        ? entry.url
        : githubReleaseUrl(version),
  };
};

/**
 * The feed as published (`public/releases.json`) or as authored
 * (`data/releases.json`): both are `{ releases: [...] }`. Entries that do not
 * parse are dropped rather than failing the whole feed; null when nothing is usable.
 */
export const parseReleaseFeed = (json: unknown): ReleaseFeed | null => {
  if (!json || typeof json !== 'object') return null;
  const list = (json as { releases?: unknown }).releases;
  if (!Array.isArray(list)) return null;
  const releases = list
    .map(parseEntry)
    .filter((entry): entry is ReleaseEntry => entry !== null)
    .sort((a, b) => compareVersions(b.version, a.version));
  if (releases.length === 0) return null;
  const latest = releases.find((entry) => !entry.draft)?.version ?? null;
  return { latest, releases };
};

export const findRelease = (feed: ReleaseFeed, version: string): ReleaseEntry | undefined =>
  feed.releases.find((entry) => compareVersions(entry.version, version) === 0);

/** What an update from `from` to `to` brings: newer than `from`, up to `to`, newest first. */
export const releasesBetween = (feed: ReleaseFeed, from: string, to: string): ReleaseEntry[] =>
  feed.releases.filter(
    (entry) => compareVersions(entry.version, from) > 0 && compareVersions(entry.version, to) <= 0
  );

/**
 * What Settings lists: everything up to the running version. A draft shows only
 * once it is the version you run (a dev build, or a release whose site entry
 * has not been finished yet).
 */
export const releasesUpTo = (feed: ReleaseFeed, current: string | null): ReleaseEntry[] =>
  feed.releases.filter((entry) =>
    current ? compareVersions(entry.version, current) <= 0 : !entry.draft
  );

export const hasNotes = (entry: ReleaseEntry): boolean =>
  NOTE_GROUPS.some((group) => entry.notes[group].length > 0);

const MONTHS = [
  'January',
  'February',
  'March',
  'April',
  'May',
  'June',
  'July',
  'August',
  'September',
  'October',
  'November',
  'December',
];

/** `2026-10-26` → `October 26, 2026`, read as a calendar date (no time zone shift). */
export const formatReleaseDate = (date: string | null): string => {
  if (!date) return '';
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date);
  if (!match) return '';
  const month = MONTHS[Number(match[2]) - 1];
  return month ? `${month} ${Number(match[3])}, ${match[1]}` : '';
};

export interface NotesMarkdownOptions {
  /** A `## v0.3.18: Title` heading per version; off for a single version's body. */
  versionHeadings?: boolean;
  /** Always lead with the summary; otherwise only when a version has no itemized notes. */
  includeSummary?: boolean;
}

/** Markdown for one or more versions: New / Improved / Fixed lists under each. */
export const entriesToMarkdown = (
  entries: ReleaseEntry[],
  { versionHeadings = entries.length > 1, includeSummary = false }: NotesMarkdownOptions = {}
): string => {
  const sectionLevel = versionHeadings ? '###' : '##';
  return entries
    .map((entry) => {
      const parts: string[] = [];
      if (versionHeadings) {
        parts.push(`## v${entry.version}${entry.title ? `: ${entry.title}` : ''}`);
      }
      if (entry.summary && (includeSummary || !hasNotes(entry))) parts.push(entry.summary);
      for (const group of NOTE_GROUPS) {
        const items = entry.notes[group];
        if (items.length === 0) continue;
        parts.push(`${sectionLevel} ${NOTE_GROUP_LABELS[group]}`);
        parts.push(items.map((item) => `- ${item}`).join('\n'));
      }
      return parts.join('\n\n');
    })
    .join('\n\n');
};

/** The previous version in the feed, for the compare link. Null for the first one. */
export const previousRelease = (feed: ReleaseFeed, version: string): ReleaseEntry | null =>
  feed.releases.find((entry) => compareVersions(entry.version, version) < 0) ?? null;

/**
 * The GitHub Release body and `latest.json` notes for one version: its summary,
 * its notes, and a compare link to the version before it.
 */
export const releaseBody = (feed: ReleaseFeed, version: string): string | null => {
  const entry = findRelease(feed, version);
  if (!entry) return null;
  const body = entriesToMarkdown([entry], { versionHeadings: false, includeSummary: true });
  const previous = previousRelease(feed, entry.version);
  const compare = previous
    ? `**Full Changelog**: ${REPOSITORY_URL}/compare/v${previous.version}...v${entry.version}`
    : '';
  return [body, compare].filter(Boolean).join('\n\n');
};

export type FetchLike = (
  url: string,
  init?: { signal?: AbortSignal; headers?: Record<string, string> }
) => Promise<{ ok: boolean; status: number; json(): Promise<unknown> }>;

/** The feed, or null when it can't be reached or read in `timeoutMs`. Never throws. */
export const fetchReleaseFeed = async (
  fetchImpl: FetchLike,
  url: string = RELEASES_URL,
  timeoutMs = 8_000
): Promise<ReleaseFeed | null> => {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const response = await fetchImpl(url, {
      signal: controller.signal,
      headers: { Accept: 'application/json' },
    });
    if (!response.ok) return null;
    return parseReleaseFeed(await response.json());
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
  }
};
