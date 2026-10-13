import { describe, expect, test } from 'bun:test';
import { getFileExtension, getPreviewKind } from './previewKind';

const TEXT = ['txt', 'md', 'json', 'csv'];

describe('getFileExtension', () => {
  test('lower-cases the last extension only', () => {
    expect(getFileExtension('Report.Final.DOCX')).toBe('docx');
    expect(getFileExtension('archive.tar.gz')).toBe('gz');
  });

  test('returns empty for files without an extension', () => {
    expect(getFileExtension('Makefile')).toBe('');
    expect(getFileExtension('')).toBe('');
  });
});

describe('getPreviewKind', () => {
  test('keeps browser-native media on the streaming players', () => {
    expect(getPreviewKind('photo.JPG', TEXT)).toBe('image');
    expect(getPreviewKind('clip.mp4', TEXT)).toBe('video');
    expect(getPreviewKind('song.flac', TEXT)).toBe('audio');
  });

  test('keeps editable text on the Monaco editor', () => {
    expect(getPreviewKind('notes.md', TEXT)).toBe('text');
    expect(getPreviewKind('data.csv', TEXT)).toBe('text');
  });

  test('routes documents, archives and non-native images to the file viewer', () => {
    expect(getPreviewKind('deck.pptx', TEXT)).toBe('viewer');
    expect(getPreviewKind('manual.pdf', TEXT)).toBe('viewer');
    expect(getPreviewKind('bundle.zip', TEXT)).toBe('viewer');
    expect(getPreviewKind('scan.tiff', TEXT)).toBe('viewer');
    expect(getPreviewKind('layers.psd', TEXT)).toBe('viewer');
  });

  test('routes unknown and extension-less files to the file viewer', () => {
    expect(getPreviewKind('blob.xyz123', TEXT)).toBe('viewer');
    expect(getPreviewKind('Makefile', TEXT)).toBe('viewer');
  });
});
