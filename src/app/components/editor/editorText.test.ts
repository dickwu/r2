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
    expect(jsonProblem('{\n  "a": 1,\n}')).toBe(
      'Invalid JSON at line 3, column 1: Trailing comma before "}"'
    );
  });

  test('jsonProblem gives a line and column on WebKit, whose errors carry no position', () => {
    const parse = JSON.parse;
    const webkit = [
      'JSON Parse error: Property name must be a string literal',
      "JSON Parse error: Expected '}'",
      'JSON Parse error: Unexpected identifier "oops"',
    ];
    try {
      for (const message of webkit) {
        JSON.parse = () => {
          throw new SyntaxError(message);
        };
        expect(jsonProblem('{\n  "name": "test", oops: 1\n}')).toBe(
          'Invalid JSON at line 2, column 19: Expected a property name in double quotes, found "o"'
        );
      }
    } finally {
      JSON.parse = parse;
    }
  });

  test('jsonProblem falls back to the engine when the scan finds nothing', () => {
    const parse = JSON.parse;
    try {
      JSON.parse = () => {
        throw new SyntaxError('Unexpected token at position 3');
      };
      expect(jsonProblem('{"a":\n1}')).toBe('Invalid JSON at line 1, column 4');
      JSON.parse = () => {
        throw new SyntaxError('JSON Parse error: Something odd');
      };
      expect(jsonProblem('{"a": 1}')).toBe('JSON Parse error: Something odd');
    } finally {
      JSON.parse = parse;
    }
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
