import { describe, expect, test } from 'bun:test';
import { documentLabel, formatBytes, jsonIndent, jsonProblem } from './editorText';

describe('editorText', () => {
  test('jsonIndent keeps the document style', () => {
    expect(jsonIndent('{\n    "a": 1\n}')).toBe(4);
    expect(jsonIndent('{\n\t"a": 1\n}')).toBe('\t');
    expect(jsonIndent('{"a":1}')).toBe(2);
  });

  test('jsonProblem points at the line and column', () => {
    expect(jsonProblem('{"a": 1}')).toBeNull();
    expect(jsonProblem('   ')).toBeNull();
    const problem = jsonProblem('{\n  "a": 1,\n}');
    expect(problem).toBeTruthy();
  });

  test('documentLabel names the document type', () => {
    expect(
      documentLabel({
        editable: true,
        mode: 'text',
        flavor: 'markdown',
        language: 'markdown',
        maxBytes: 1,
      })
    ).toBe('Markdown');
    expect(
      documentLabel({
        editable: true,
        mode: 'text',
        flavor: 'delimited',
        language: 'plaintext',
        delimiter: '\t',
        maxBytes: 1,
      })
    ).toBe('TSV');
    expect(
      documentLabel({
        editable: true,
        mode: 'text',
        flavor: 'code',
        language: 'typescript',
        maxBytes: 1,
      })
    ).toBe('TypeScript');
    expect(documentLabel({ editable: true, mode: 'image', mime: 'image/webp', maxBytes: 1 })).toBe(
      'WEBP'
    );
  });

  test('formatBytes', () => {
    expect(formatBytes(512)).toBe('512 B');
    expect(formatBytes(2048)).toBe('2.0 KB');
    expect(formatBytes(5 * 1024 * 1024)).toBe('5.0 MB');
  });
});
