import { describe, expect, test } from 'bun:test';
import { findJsonSyntaxError, lineAndColumn } from './jsonSyntax';

const parses = (text: string) => {
  try {
    JSON.parse(text);
    return true;
  } catch {
    return false;
  }
};

const at = (text: string) => {
  const found = findJsonSyntaxError(text);
  return found ? lineAndColumn(text, found.offset) : null;
};

describe('findJsonSyntaxError', () => {
  test('accepts everything JSON.parse accepts', () => {
    const valid = [
      '{}',
      '[]',
      ' \n\t{"a": [1, -2.5e+3, 0, 0.1E-2, true, false, null, "x\\n\\u00e9\\"\\\\"]}\r\n',
      '"just a string"',
      '42',
      '{"nested": {"deep": [[], {}, [{"k": "v"}]]}}',
    ];
    for (const text of valid) {
      expect(parses(text)).toBe(true);
      expect(findJsonSyntaxError(text)).toBeNull();
    }
  });

  test('rejects everything JSON.parse rejects', () => {
    const invalid = [
      '{a: 1}',
      "{'a': 1}",
      '{"a": 1,}',
      '[1, 2,]',
      '{"a" 1}',
      '{"a": 1 "b": 2}',
      '[1 2]',
      '{"a": 01}',
      '{"a": 1.}',
      '{"a": -}',
      '{"a": tru}',
      '{"a": "unterminated}',
      '{"a": "bad \\x escape"}',
      '{"a": "\\u12G4"}',
      '{"a": "line\nbreak"}',
      '{"a": 1}}',
      '{"a": 1',
      '[',
      'undefined',
      '{"a": NaN}',
    ];
    for (const text of invalid) {
      expect(parses(text)).toBe(false);
      expect(findJsonSyntaxError(text)).not.toBeNull();
    }
  });

  test('points at the character that breaks the document', () => {
    // The case from the live test: an unquoted key on line 2, column 19.
    expect(at('{\n  "name": "test", oops: 1\n}')).toEqual({ line: 2, column: 19 });
    expect(at('{\n  "a": 1,\n}')).toEqual({ line: 3, column: 1 });
    expect(at('[1, 2,]')).toEqual({ line: 1, column: 7 });
    expect(at('{"a": 1 "b": 2}')).toEqual({ line: 1, column: 9 });
    expect(at('{"a": tru}')).toEqual({ line: 1, column: 10 });
    expect(at('{"a": 1')).toEqual({ line: 1, column: 8 });
    expect(at('{"a": 1}\n}')).toEqual({ line: 2, column: 1 });
  });

  test('explains what was expected', () => {
    expect(findJsonSyntaxError('{a: 1}')?.reason).toBe(
      'Expected a property name in double quotes, found "a"'
    );
    expect(findJsonSyntaxError('{"a": 1,}')?.reason).toBe('Trailing comma before "}"');
    expect(findJsonSyntaxError('{"a": 1')?.reason).toBe(
      'Expected "," or "}", found the end of the document'
    );
  });
});
