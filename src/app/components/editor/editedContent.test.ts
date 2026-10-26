import { describe, expect, test } from 'bun:test';
import {
  copyFileName,
  copyFilters,
  downloadEditedCopy,
  editedText,
  imageEncodingProblem,
  localFileName,
  saveFailure,
  type CopyDependencies,
} from './editedContent';
import { decodeText, type TextFormat } from './textCodec';
import { parseDelimited, serializeDelimited } from './delimited';
import { saveWithVersionCheck } from './saveFlow';

const utf8 = (s: string) => new TextEncoder().encode(s);
const list = (bytes: Uint8Array) => Array.from(bytes);

/** What Save would store for a string body, as bytes. */
const stored = (body: string | Uint8Array) => (typeof body === 'string' ? utf8(body) : body);

/** A local disk: records what was written where. */
function disk(choice: string | null = '/Users/me/Downloads/notes.md') {
  const written = new Map<string, Uint8Array>();
  const asked: { defaultPath: string; extensions: string[] }[] = [];
  const deps: CopyDependencies = {
    pickPath: async (defaultPath, filters) => {
      asked.push({ defaultPath, extensions: filters.flatMap((f) => f.extensions) });
      return choice;
    },
    writeFile: async (path, bytes) => {
      written.set(path, bytes);
    },
  };
  return { deps, written, asked };
}

describe('editedText: the copy has the bytes Save would upload', () => {
  const cases: [string, Uint8Array][] = [
    ['UTF-8, LF', utf8('héllo\nwörld\n')],
    ['UTF-8, CRLF', utf8('a\r\nb\r\n')],
    ['UTF-8 with BOM, CRLF', new Uint8Array([0xef, 0xbb, 0xbf, ...utf8('x\r\ny\r\n')])],
    ['UTF-16 LE with BOM', new Uint8Array([0xff, 0xfe, 0x68, 0x00, 0x0d, 0x00, 0x0a, 0x00])],
    ['UTF-16 BE with BOM', new Uint8Array([0xfe, 0xff, 0x00, 0x68, 0x00, 0x0a])],
  ];
  for (const [name, original] of cases) {
    test(`${name}: an edit keeps the format and matches the upload`, () => {
      const decoded = decodeText(original);
      if (!decoded.ok) throw new Error(decoded.reason);
      const unchanged = editedText(decoded.text, decoded.format);
      expect(list(unchanged.bytes)).toEqual(list(original));
      expect(list(stored(unchanged.body))).toEqual(list(unchanged.bytes));

      const edited = editedText(`${decoded.text}more\n`, decoded.format);
      expect(list(stored(edited.body))).toEqual(list(edited.bytes));
      const again = decodeText(edited.bytes);
      expect(again.ok && again.format).toEqual(decoded.format);
      expect(again.ok && again.text).toBe(`${decoded.text}more\n`);
    });
  }

  test('CSV edited in table mode is written with the file’s own line endings and BOM', () => {
    const format: TextFormat = { encoding: 'utf-8', bom: true, eol: '\r\n' };
    const table = parseDelimited('name,city\nAda,London\n', ',');
    table.rows[1][1] = 'Paris, France';
    const { bytes } = editedText(serializeDelimited(table, ','), format);
    expect(list(bytes)).toEqual([
      0xef,
      0xbb,
      0xbf,
      ...utf8('name,city\r\nAda,"Paris, France"\r\n'),
    ]);
  });
});

describe('imageEncodingProblem', () => {
  test('the copy is written only in the original image format', () => {
    expect(imageEncodingProblem('image/png', 'image/png')).toBeNull();
    expect(imageEncodingProblem('image/jpeg', 'image/jpeg')).toBeNull();
    expect(imageEncodingProblem('image/png', 'image/webp')).toContain('WEBP');
  });
});

describe('downloadEditedCopy', () => {
  test('offers the object’s own name and extension, and writes the exact bytes', async () => {
    const { deps, written, asked } = disk('/tmp/README.md');
    const bytes = utf8('# Title\n');
    const outcome = await downloadEditedCopy('docs/guide/README.md', bytes, deps);
    expect(outcome).toEqual({ status: 'saved', path: '/tmp/README.md' });
    expect(asked).toEqual([{ defaultPath: 'README.md', extensions: ['md'] }]);
    expect(list(written.get('/tmp/README.md')!)).toEqual(list(bytes));
  });

  test('cancelling the dialog writes nothing', async () => {
    const { deps, written } = disk(null);
    expect(await downloadEditedCopy('a.csv', utf8('x'), deps)).toEqual({ status: 'cancelled' });
    expect(written.size).toBe(0);
  });

  test('names and filters', () => {
    expect(copyFileName('a/b/photo.webp')).toBe('photo.webp');
    expect(copyFileName('folder/')).toBe('edited-file');
    expect(copyFilters('Dockerfile')).toEqual([]);
    expect(copyFilters('.env')).toEqual([]);
    expect(copyFilters('data.tsv')).toEqual([{ name: 'TSV file', extensions: ['tsv'] }]);
    expect(localFileName('C:\\Users\\me\\a.json')).toBe('a.json');
    expect(localFileName('/home/me/b.md')).toBe('b.md');
  });
});

describe('a read-only key: 403 on save -> download the edited copy', () => {
  test('the failure offers the copy and makes it the main action', async () => {
    const format: TextFormat = { encoding: 'utf-8', bom: false, eol: '\r\n' };
    const edited = editedText('id,name\n1,Ada\n', format);
    const uploads: (string | Uint8Array)[] = [];
    const outcome = await saveWithVersionCheck('"v1"', {
      currentEtag: async () => '"v1"',
      upload: async () => {
        uploads.push(edited.body);
        throw new Error('HTTP 403: AccessDenied: Access Denied');
      },
    });
    expect(outcome.status).toBe('error');
    if (outcome.status !== 'error') return;
    const failure = saveFailure(outcome.kind, outcome.message);
    expect(failure.offerCopy).toBe(true);
    expect(failure.readOnly).toBe(true);
    expect(failure.text).toContain('still in the editor');

    // The download path then writes the same bytes the upload carried.
    const { deps, written } = disk('/tmp/people.csv');
    await downloadEditedCopy('exports/people.csv', edited.bytes, deps);
    expect(list(written.get('/tmp/people.csv')!)).toEqual(list(stored(uploads[0])));
    expect(list(written.get('/tmp/people.csv')!)).toEqual(list(utf8('id,name\r\n1,Ada\r\n')));
  });

  test('other failures offer the copy without switching to read-only', () => {
    for (const kind of ['conflict', 'other', 'size', 'too-large'] as const) {
      expect(saveFailure(kind, 'x')).toEqual({ text: 'x', offerCopy: true, readOnly: false });
    }
    // An image this system cannot encode cannot be downloaded either.
    expect(saveFailure('encode', 'x').offerCopy).toBe(false);
  });
});
