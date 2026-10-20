import { describe, expect, test } from 'bun:test';
import {
  getContentTypePreview,
  getDetectedPreview,
  getExtensionPreview,
  getFileExtension,
  getPreviewKind,
  isSameRendering,
  toViewerFilename,
  type DetectedType,
} from './previewKind';

const TEXT = ['txt', 'md', 'json', 'xml', 'csv'];

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

/** `kind:extension` of the preview a Content-Type calls for, or null. */
function routeOf(contentType: string | null | undefined): string | null {
  const target = getContentTypePreview(contentType, TEXT);
  return target && `${target.kind}:${target.extension}`;
}

describe('getContentTypePreview', () => {
  test('streams extension-less media in the native players', () => {
    expect(routeOf('image/jpeg')).toBe('image:jpg');
    expect(routeOf('video/quicktime')).toBe('video:mov');
    expect(routeOf('audio/mpeg')).toBe('audio:mp3');
  });

  test('ignores case and parameters, but keeps the header for saving', () => {
    expect(routeOf('Image/PNG')).toBe('image:png');
    expect(getContentTypePreview('application/json; charset=utf-8', TEXT)).toEqual({
      kind: 'text',
      extension: 'json',
      contentType: 'application/json; charset=utf-8',
    });
  });

  test('opens text, JSON and XML types in the editor', () => {
    expect(routeOf('text/plain')).toBe('text:txt');
    expect(routeOf('application/ld+json')).toBe('text:json');
    expect(routeOf('application/rss+xml')).toBe('text:xml');
    expect(routeOf('image/svg+xml')).toBe('image:svg');
  });

  test('routes formats the webview cannot play to the file viewer', () => {
    expect(routeOf('application/pdf')).toBe('viewer:pdf');
    expect(routeOf('image/tiff')).toBe('viewer:tiff');
    expect(routeOf('application/vnd.openxmlformats-officedocument.wordprocessingml.document')).toBe(
      'viewer:docx'
    );
  });

  test('falls back on the family for unlisted subtypes', () => {
    expect(routeOf('image/x-portable-pixmap')).toBe('image:');
    expect(routeOf('video/x-matroska')).toBe('video:');
    expect(routeOf('audio/webm')).toBe('audio:');
    expect(routeOf('text/x-python')).toBe('text:');
  });

  test('returns null for types that name no previewable format', () => {
    for (const contentType of [
      'application/octet-stream',
      'binary/octet-stream',
      'constructor/x',
      'image',
      '',
      null,
      undefined,
    ]) {
      expect(routeOf(contentType)).toBeNull();
    }
  });
});

const VIEWER = ['pdf', 'docx', 'zip', 'tiff', 'heic', 'gz'];
const isViewerFormat = (extension: string) => VIEWER.includes(extension);

function detected(
  mime: string,
  extension: string,
  contentType: string | null = 'application/octet-stream'
): DetectedType {
  return { mime, extension, name: mime, contentType };
}

/** `kind:extension` of the preview a detection calls for, or null. */
function detectedRouteOf(detection: DetectedType): string | null {
  const target = getDetectedPreview(detection, TEXT, isViewerFormat);
  return target && `${target.kind}:${target.extension}`;
}

describe('getDetectedPreview', () => {
  test('routes a specific detection by its extension, whatever the stored type says', () => {
    expect(detectedRouteOf(detected('image/jpeg', 'jpg'))).toBe('image:jpg');
    expect(detectedRouteOf(detected('application/pdf', 'pdf', 'image/png'))).toBe('viewer:pdf');
    expect(detectedRouteOf(detected('application/json', 'json', null))).toBe('text:json');
  });

  test('lets the stored type refine a detection that only names a family or container', () => {
    expect(detectedRouteOf(detected('text/plain; charset=utf-8', 'txt', 'text/csv'))).toBe(
      'text:csv'
    );
    expect(
      detectedRouteOf(
        detected(
          'application/zip',
          'zip',
          'application/vnd.openxmlformats-officedocument.wordprocessingml.document'
        )
      )
    ).toBe('viewer:docx');
    expect(detectedRouteOf(detected('application/octet-stream', '', 'image/png'))).toBe(
      'image:png'
    );
  });

  test('falls back on the generic detection when the stored type adds nothing', () => {
    expect(detectedRouteOf(detected('text/plain; charset=utf-8', 'txt'))).toBe('text:txt');
    expect(detectedRouteOf(detected('application/zip', 'zip', null))).toBe('viewer:zip');
  });

  test('returns null when nothing renders the content', () => {
    expect(detectedRouteOf(detected('application/octet-stream', ''))).toBeNull();
    expect(detectedRouteOf(detected('application/x-empty', 'empty', null))).toBeNull();
    expect(detectedRouteOf(detected('image/x-canon-cr2', 'cr2'))).toBeNull();
  });

  test('keeps the stored type for saving, or else the detected one', () => {
    expect(
      getDetectedPreview(detected('application/json', 'json', 'text/plain'), TEXT, isViewerFormat)
        ?.contentType
    ).toBe('text/plain');
    expect(
      getDetectedPreview(detected('application/json', 'json', null), TEXT, isViewerFormat)
        ?.contentType
    ).toBe('application/json');
  });
});

describe('isSameRendering', () => {
  const preview = (extension: string) => getExtensionPreview(extension, TEXT);

  test('native players and the editor take any format of their kind', () => {
    expect(isSameRendering(preview('jpg'), preview('png'))).toBe(true);
    expect(isSameRendering(preview('mp4'), preview('webm'))).toBe(true);
    expect(isSameRendering(preview('txt'), preview('json'))).toBe(true);
  });

  test('the viewer renders the format it is told, so formats must match', () => {
    expect(isSameRendering(preview('pdf'), preview('pdf'))).toBe(true);
    expect(isSameRendering(preview('tif'), preview('tiff'))).toBe(true);
    expect(isSameRendering(preview('tiff'), preview('heic'))).toBe(false);
    expect(isSameRendering(preview('docx'), preview('zip'))).toBe(false);
  });

  test('a different surface is a different rendering', () => {
    expect(isSameRendering(preview('jpg'), preview('heic'))).toBe(false);
    expect(isSameRendering(preview('pdf'), preview('html'))).toBe(false);
  });
});

describe('toViewerFilename', () => {
  test('replaces the characters the viewer treats as URL delimiters', () => {
    expect(toViewerFilename('Invoice #123.pdf')).toBe('Invoice _123.pdf');
    expect(toViewerFilename('What?.docx')).toBe('What_.docx');
    expect(toViewerFilename('plain.xlsx')).toBe('plain.xlsx');
  });

  test('appends the rendered format when the name does not end in it', () => {
    expect(toViewerFilename('6c4c4cb6-6fd5-446b-9ca3-179878730551', 'pdf')).toBe(
      '6c4c4cb6-6fd5-446b-9ca3-179878730551.pdf'
    );
    expect(toViewerFilename('scan.backup', 'tiff')).toBe('scan.backup.tiff');
    expect(toViewerFilename('Report.PDF', 'pdf')).toBe('Report.PDF');
    expect(toViewerFilename('Invoice #3.pdf', 'pdf')).toBe('Invoice _3.pdf');
  });
});
