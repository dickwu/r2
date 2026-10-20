import { describe, expect, test } from 'bun:test';
import { isFileViewerFormat } from './fileViewerFormats';
import {
  CONTENT_TYPE_EXTENSIONS,
  NATIVE_AUDIO_EXTENSIONS,
  NATIVE_IMAGE_EXTENSIONS,
  NATIVE_VIDEO_EXTENSIONS,
} from './previewKind';

describe('isFileViewerFormat', () => {
  test('knows what the bundled full preset renders', () => {
    for (const extension of ['pdf', 'docx', 'xlsx', 'pptx', 'zip', 'tiff', 'heic', 'psd']) {
      expect(isFileViewerFormat(extension)).toBe(true);
    }
    expect(isFileViewerFormat('')).toBe(false);
    expect(isFileViewerFormat('xyz123')).toBe(false);
  });

  test('every format a Content-Type maps to has a preview surface', () => {
    const native = [
      ...NATIVE_IMAGE_EXTENSIONS,
      ...NATIVE_VIDEO_EXTENSIONS,
      ...NATIVE_AUDIO_EXTENSIONS,
    ];
    const unrenderable = Object.entries(CONTENT_TYPE_EXTENSIONS).filter(
      ([, extension]) => !native.includes(extension) && !isFileViewerFormat(extension)
    );
    expect(unrenderable).toEqual([]);
  });
});
