import { describe, expect, test } from 'bun:test';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const appPath = (relativePath: string) => fileURLToPath(new URL(relativePath, import.meta.url));
const SRC_DIR = appPath('../');
const FONTS_DIR = appPath('../../public/fonts/');

// The families fonts.css self-hosts: the variable globals.css reads each one through, the slug its
// files in public/fonts/ are named with, and the weight range every one of its faces declares.
const FAMILIES = [
  { family: 'Inter', variable: '--font-inter', slug: 'inter', weightRange: '400 700' },
  {
    family: 'Inter Tight',
    variable: '--font-inter-tight',
    slug: 'inter-tight',
    weightRange: '500 700',
  },
  {
    family: 'JetBrains Mono',
    variable: '--font-jetbrains-mono',
    slug: 'jetbrains-mono',
    weightRange: '400 600',
  },
];

// Source files, and an import or require that loads next/font/google. Only the module loading
// syntax counts, so the module name inside other strings (such as this file's test names) does not.
const SOURCE_FILE = /\.(?:jsx?|tsx?|[cm][jt]s)$/;
const NEXT_FONT_GOOGLE_IMPORT = /\b(?:from|import|require)\s*\(?\s*['"]next\/font\/google\b/;

const readSource = (relativePath: string) => readFileSync(appPath(relativePath), 'utf8');

// Comments are dropped so that text inside them never counts as a face or a font URL.
const readFontsCss = () => readSource('./fonts.css').replace(/\/\*[\s\S]*?\*\//g, '');

// Likewise for the layout, where a commented-out import or preload() call must not count. A `//`
// only starts a comment at the start of a line or after whitespace, so `https://` is left alone.
const readLayout = () =>
  readSource('./layout.tsx').replace(/\/\*[\s\S]*?\*\/|(^|\s)\/\/.*$/gm, '$1');

const listWoff2Files = () => readdirSync(FONTS_DIR).filter((file) => file.endsWith('.woff2'));

// Whether a URL is a root-relative /fonts/<file> path to a file that exists in public/fonts/.
function isInFontsDir(url: string) {
  const file = url.match(/^\/fonts\/([^/]+)$/)?.[1];
  return file !== undefined && existsSync(join(FONTS_DIR, file));
}

// The value of one descriptor in a rule body, without surrounding quotes ('' when it is absent).
function descriptor(body: string, name: string) {
  const value = body.match(new RegExp(`(?:^|;)\\s*${name}\\s*:\\s*([^;]*)`))?.[1] ?? '';
  return value.trim().replace(/^(['"])(.*)\1$/, '$2');
}

function parseFontFaces(css: string) {
  return [...css.matchAll(/@font-face\s*\{([^}]*)\}/g)].map(([, body]) => ({
    family: descriptor(body, 'font-family'),
    weight: descriptor(body, 'font-weight'),
    display: descriptor(body, 'font-display'),
    urls: [...body.matchAll(/url\((['"]?)(.*?)\1\)/g)].map(([, , url]) => url),
  }));
}

describe('self-hosted fonts', () => {
  test('no source file under src/ imports next/font/google (a build-time Google fetch)', () => {
    const offenders = readdirSync(SRC_DIR, { encoding: 'utf8', recursive: true })
      .filter((file) => SOURCE_FILE.test(file))
      .filter((file) => NEXT_FONT_GOOGLE_IMPORT.test(readFileSync(join(SRC_DIR, file), 'utf8')))
      .map((file) => `src/${file}`);
    expect(offenders).toEqual([]);
  });

  test('every font URL in fonts.css points to a file in public/fonts', () => {
    const urls = parseFontFaces(readFontsCss()).flatMap((face) => face.urls);
    expect(urls.length).toBeGreaterThan(0);
    const broken = urls.filter((url) => !isInFontsDir(url));
    expect(broken).toEqual([]);
  });

  test('every woff2 in public/fonts is referenced by fonts.css', () => {
    const referenced = new Set(parseFontFaces(readFontsCss()).flatMap((face) => face.urls));
    const files = listWoff2Files();
    expect(files.length).toBeGreaterThan(0);
    const unreferenced = files.filter((file) => !referenced.has(`/fonts/${file}`));
    expect(unreferenced).toEqual([]);
  });

  test('every woff2 in public/fonts is a WOFF2 file, not an error page saved under that name', () => {
    const notWoff2 = listWoff2Files().filter(
      (file) => readFileSync(join(FONTS_DIR, file)).subarray(0, 4).toString('latin1') !== 'wOF2'
    );
    expect(notWoff2).toEqual([]);
  });

  test('fonts.css declares the variables globals.css reads on :root, with their fallback faces', () => {
    const globals = readSource('./globals.css');
    const css = readFontsCss();
    const rootDeclarations = [...css.matchAll(/:root\s*\{([^}]*)\}/g)]
      .map(([, body]) => body)
      .join('\n');
    const faceFamilies = parseFontFaces(css).map((face) => face.family);
    for (const { family, variable } of FAMILIES) {
      expect(globals).toContain(`var(${variable})`);
      expect(rootDeclarations).toMatch(
        new RegExp(`${variable}:\\s*(['"])${family}\\1,\\s*(['"])${family} Fallback\\2;`)
      );
      expect(faceFamilies).toContain(`${family} Fallback`);
    }
  });

  test('every web font face is one of the families, with font-display: swap and its weight range', () => {
    const faces = parseFontFaces(readFontsCss()).filter(
      (face) => !face.family.endsWith(' Fallback')
    );
    // A misspelled family would leave that subset's text in the fallback font without any error.
    const families = new Set(FAMILIES.map(({ family }) => family));
    const unknownFamily = faces
      .filter((face) => !families.has(face.family))
      .flatMap((face) => face.urls);
    expect(unknownFamily).toEqual([]);
    const notSwap = faces.filter((face) => face.display !== 'swap').flatMap((face) => face.urls);
    expect(notSwap).toEqual([]);
    for (const { family, weightRange } of FAMILIES) {
      const familyFaces = faces.filter((face) => face.family === family);
      expect(familyFaces.length).toBeGreaterThan(0);
      const otherRange = familyFaces
        .filter((face) => face.weight !== weightRange)
        .flatMap((face) => face.urls);
      expect(otherRange).toEqual([]);
    }
  });

  test("layout.tsx imports fonts.css and preloads each family's latin file with react-dom's preload()", () => {
    const layout = readLayout();
    expect(layout).toMatch(/^import '@\/app\/fonts\.css';$/m);
    expect(layout).toMatch(/^import \{[^}]*\bpreload\b[^}]*\} from 'react-dom';$/m);
    const preloads = [...layout.matchAll(/\bpreload\(\s*'([^']*)',\s*\{([^}]*)\}\s*\)/g)].map(
      ([, href, options]) => ({ href, options })
    );
    const preloadedHrefs = preloads.map((call) => call.href);
    const faces = parseFontFaces(readFontsCss());
    for (const { family, slug } of FAMILIES) {
      const href = `/fonts/${slug}-latin.woff2`;
      expect(faces.some((face) => face.family === family && face.urls.includes(href))).toBe(true);
      expect(preloadedHrefs).toContain(href);
      const options = preloads.find((call) => call.href === href)?.options ?? '';
      expect(options).toMatch(/\bas:\s*'font'/);
      expect(options).toMatch(/\btype:\s*'font\/woff2'/);
    }
    const missing = preloadedHrefs.filter((href) => !isInFontsDir(href));
    expect(missing).toEqual([]);
  });
});
