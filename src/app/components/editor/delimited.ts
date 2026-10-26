/**
 * CSV/TSV for the table editor (RFC 4180 quoting). Parsing then serializing an
 * unedited file gives back the same text, so switching between the table and
 * text views never rewrites a file the user did not touch.
 */

export type Delimiter = ',' | '\t' | ';';

export interface DelimitedTable {
  rows: string[][];
  /** Whether the text ended with a line break (kept on save). */
  trailingNewline: boolean;
}

/** Parses delimited text whose line breaks are '\n' (the editor's normal form). */
export function parseDelimited(text: string, delimiter: Delimiter): DelimitedTable {
  const rows: string[][] = [];
  let row: string[] = [];
  let field = '';
  let quoted = false;
  let i = 0;
  const trailingNewline = text.endsWith('\n');
  const source = trailingNewline ? text.slice(0, -1) : text;

  while (i < source.length) {
    const c = source[i];
    if (quoted) {
      if (c === '"') {
        if (source[i + 1] === '"') {
          field += '"';
          i += 2;
          continue;
        }
        quoted = false;
        i++;
        continue;
      }
      field += c;
      i++;
      continue;
    }
    if (c === '"' && field === '') {
      quoted = true;
      i++;
    } else if (c === delimiter) {
      row.push(field);
      field = '';
      i++;
    } else if (c === '\n') {
      row.push(field);
      rows.push(row);
      row = [];
      field = '';
      i++;
    } else {
      field += c;
      i++;
    }
  }
  if (source.length > 0 || trailingNewline) {
    row.push(field);
    rows.push(row);
  }
  return { rows, trailingNewline };
}

function quoteField(field: string, delimiter: Delimiter): string {
  if (
    field.includes('"') ||
    field.includes(delimiter) ||
    field.includes('\n') ||
    field.includes('\r')
  ) {
    return `"${field.replace(/"/g, '""')}"`;
  }
  return field;
}

/** Serializes rows back to text with '\n' line breaks. */
export function serializeDelimited(table: DelimitedTable, delimiter: Delimiter): string {
  const body = table.rows
    .map((row) => row.map((field) => quoteField(field, delimiter)).join(delimiter))
    .join('\n');
  return table.trailingNewline ? `${body}\n` : body;
}

/** The widest row's column count. */
export function columnCount(rows: readonly string[][]): number {
  return rows.reduce((max, row) => Math.max(max, row.length), 0);
}

/** Guesses the delimiter of a `.csv` that may use semicolons (common in some locales). */
export function sniffDelimiter(text: string, fallback: Delimiter): Delimiter {
  if (fallback === '\t') return fallback;
  const firstLine = text.slice(0, text.indexOf('\n') === -1 ? undefined : text.indexOf('\n'));
  const commas = firstLine.split(',').length - 1;
  const semicolons = firstLine.split(';').length - 1;
  return semicolons > commas ? ';' : ',';
}
