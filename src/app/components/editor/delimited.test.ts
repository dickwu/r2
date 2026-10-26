import { describe, expect, test } from 'bun:test';
import { columnCount, parseDelimited, serializeDelimited, sniffDelimiter } from './delimited';

describe('parseDelimited / serializeDelimited', () => {
  test('an unedited file serializes back to the same text', () => {
    for (const text of [
      'a,b,c\n1,2,3\n',
      'name,quote\n"Smith, J","He said ""hi"""\n',
      'multi,line\n"one\ntwo",x',
      'a,,c\n,,\n',
      ' spaced , cells \n',
    ]) {
      expect(serializeDelimited(parseDelimited(text, ','), ',')).toBe(text);
    }
  });

  test('quoted fields keep delimiters, quotes and line breaks', () => {
    const table = parseDelimited('"a,b","c""d","e\nf"\n', ',');
    expect(table.rows).toEqual([['a,b', 'c"d', 'e\nf']]);
    expect(table.trailingNewline).toBe(true);
  });

  test('an edited cell is quoted only when it needs to be', () => {
    const table = parseDelimited('a,b\n', ',');
    table.rows[0][1] = 'x,y';
    expect(serializeDelimited(table, ',')).toBe('a,"x,y"\n');
    table.rows[0][1] = 'plain';
    expect(serializeDelimited(table, ',')).toBe('a,plain\n');
  });

  test('TSV and semicolon files use their own separator', () => {
    expect(parseDelimited('a\tb\n', '\t').rows).toEqual([['a', 'b']]);
    expect(parseDelimited('a;b\n', ';').rows).toEqual([['a', 'b']]);
  });

  test('columnCount is the widest row', () => {
    expect(columnCount([['a'], ['a', 'b', 'c'], []])).toBe(3);
    expect(columnCount([])).toBe(0);
  });

  test('sniffDelimiter spots semicolon CSV', () => {
    expect(sniffDelimiter('a;b;c\n1;2;3', ',')).toBe(';');
    expect(sniffDelimiter('a,b\n', ',')).toBe(',');
    expect(sniffDelimiter('a;b', '\t')).toBe('\t');
  });
});
