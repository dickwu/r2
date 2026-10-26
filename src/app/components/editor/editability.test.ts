import { describe, expect, test } from 'bun:test';
import {
  MAX_IMAGE_EDIT_BYTES,
  MAX_TEXT_EDIT_BYTES,
  getEditability,
  guessContentType,
  isTextMime,
  monacoLanguageFor,
} from './editability';
import type { PreviewTarget } from '@/app/components/preview/previewKind';

const text = (name: string, target: PreviewTarget | null = null, size?: number) =>
  getEditability(name, target, size);

describe('getEditability', () => {
  test('code and plain text open in the code editor with their language', () => {
    const ts = text('src/index.ts');
    expect(ts.editable && ts.mode === 'text' && ts.flavor).toBe('code');
    expect(ts.editable && ts.mode === 'text' && ts.language).toBe('typescript');
    const log = text('server.log');
    expect(log.editable && log.mode === 'text' && log.language).toBe('plaintext');
  });

  test('markdown, JSON and delimited files get their own flavors', () => {
    const md = text('README.md');
    expect(md.editable && md.mode === 'text' && md.flavor).toBe('markdown');
    const json = text('package.json');
    expect(json.editable && json.mode === 'text' && json.flavor).toBe('json');
    const csv = text('data.csv');
    expect(csv.editable && csv.mode === 'text' && csv.flavor).toBe('delimited');
    expect(csv.editable && csv.mode === 'text' && csv.delimiter).toBe(',');
    const tsv = text('data.tsv');
    expect(tsv.editable && tsv.mode === 'text' && tsv.delimiter).toBe('\t');
  });

  test('well-known extensionless names are text', () => {
    expect(text('Dockerfile').editable).toBe(true);
    expect(text('configs/Makefile').editable).toBe(true);
    expect(text('LICENSE').editable).toBe(true);
  });

  test('raster images the canvas can write back are editable in their own format', () => {
    for (const [name, mime] of [
      ['photo.jpg', 'image/jpeg'],
      ['photo.JPEG', 'image/jpeg'],
      ['logo.png', 'image/png'],
      ['hero.webp', 'image/webp'],
    ]) {
      const result = text(name);
      expect(result.editable && result.mode === 'image' && result.mime).toBe(mime);
    }
  });

  test('formats that cannot be saved back stay view-only with a reason', () => {
    for (const name of [
      'anim.gif',
      'icon.ico',
      'pic.heic',
      'movie.mp4',
      'song.mp3',
      'doc.pdf',
      'report.docx',
      'sheet.xlsx',
      'bundle.zip',
    ]) {
      const result = text(name);
      expect(result.editable).toBe(false);
      if (!result.editable) expect(result.reason.length).toBeGreaterThan(10);
    }
  });

  test('a file too large to edit says so', () => {
    const big = text('dump.sql', null, MAX_TEXT_EDIT_BYTES + 1);
    expect(big.editable).toBe(false);
    if (!big.editable) expect(big.reason).toContain('MB');
    expect(text('huge.png', null, MAX_IMAGE_EDIT_BYTES + 1).editable).toBe(false);
    expect(text('ok.png', null, MAX_IMAGE_EDIT_BYTES).editable).toBe(true);
  });

  test('a nameless object falls back to what its content was identified as', () => {
    const asText = text('object-without-extension', {
      kind: 'text',
      extension: 'json',
      contentType: 'application/json',
      detectedType: 'application/json',
    });
    expect(asText.editable && asText.mode).toBe('text');
    const asImage = text('blob', {
      kind: 'image',
      extension: 'png',
      contentType: 'image/png',
      detectedType: 'image/png',
    });
    expect(asImage.editable && asImage.mode === 'image' && asImage.mime).toBe('image/png');
    expect(text('blob', { kind: 'viewer', extension: 'pdf' }).editable).toBe(false);
    expect(text('mystery').editable).toBe(false);
  });
});

describe('helpers', () => {
  test('isTextMime accepts text families and structured text types', () => {
    expect(isTextMime('text/plain; charset=utf-8')).toBe(true);
    expect(isTextMime('application/json')).toBe(true);
    expect(isTextMime('application/vnd.api+json')).toBe(true);
    expect(isTextMime('image/svg+xml')).toBe(true);
    expect(isTextMime('application/octet-stream')).toBe(false);
    expect(isTextMime(null)).toBe(false);
  });

  test('monacoLanguageFor maps extensions and defaults to plain text', () => {
    expect(monacoLanguageFor('a.py')).toBe('python');
    expect(monacoLanguageFor('a.yml')).toBe('yaml');
    expect(monacoLanguageFor('a.unknownext')).toBe('plaintext');
  });

  test('guessContentType gives a sensible type for a new upload', () => {
    expect(guessContentType('a.json')).toBe('application/json');
    expect(guessContentType('a.html')).toBe('text/html');
    expect(guessContentType('notes.txt')).toContain('text/plain');
  });
});
