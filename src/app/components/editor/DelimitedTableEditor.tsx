'use client';

import { useMemo } from 'react';
import { Button, Tooltip } from 'antd';
import { DeleteOutlined, InsertRowBelowOutlined, InsertRowRightOutlined } from '@ant-design/icons';
import {
  columnCount,
  parseDelimited,
  serializeDelimited,
  type Delimiter,
} from '@/app/components/editor/delimited';

/** Larger tables are edited as text: thousands of inputs would make typing lag. */
export const MAX_TABLE_ROWS = 2000;
export const MAX_TABLE_COLUMNS = 100;

/** Whether `text` is small enough for the table view (null) or why not. */
export function tableUnavailableReason(text: string, delimiter: Delimiter): string | null {
  const { rows } = parseDelimited(text, delimiter);
  if (rows.length > MAX_TABLE_ROWS) {
    return `Table view handles up to ${MAX_TABLE_ROWS.toLocaleString()} rows; this file has ${rows.length.toLocaleString()}. Edit it as text.`;
  }
  if (columnCount(rows) > MAX_TABLE_COLUMNS) {
    return `Table view handles up to ${MAX_TABLE_COLUMNS} columns. Edit it as text.`;
  }
  return null;
}

interface Props {
  text: string;
  delimiter: Delimiter;
  height: number;
  onChange: (text: string) => void;
}

/** CSV/TSV as an editable grid; every edit writes the whole text back. */
export default function DelimitedTableEditor({ text, delimiter, height, onChange }: Props) {
  const table = useMemo(() => parseDelimited(text, delimiter), [text, delimiter]);
  const columns = Math.max(columnCount(table.rows), 1);

  const commit = (rows: string[][]) => onChange(serializeDelimited({ ...table, rows }, delimiter));

  const setCell = (row: number, col: number, value: string) => {
    const rows = table.rows.map((r) => [...r]);
    while (rows[row].length <= col) rows[row].push('');
    rows[row][col] = value;
    commit(rows);
  };

  const addRow = () => commit([...table.rows, Array<string>(columns).fill('')]);
  const addColumn = () =>
    commit(
      (table.rows.length ? table.rows : [[]]).map((r) => [
        ...r,
        ...Array<string>(columns - r.length).fill(''),
        '',
      ])
    );
  const deleteRow = (index: number) => commit(table.rows.filter((_, i) => i !== index));

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 8, height }}>
      <div className="file-editor-toolbar">
        <Button size="small" icon={<InsertRowBelowOutlined />} onClick={addRow}>
          Add row
        </Button>
        <Button size="small" icon={<InsertRowRightOutlined />} onClick={addColumn}>
          Add column
        </Button>
        <span className="file-editor-status">
          {table.rows.length.toLocaleString()} rows × {columns} columns
        </span>
      </div>
      <div className="file-editor-pane file-editor-table" style={{ flex: 1 }}>
        <table>
          <thead>
            <tr>
              <th className="row-number" aria-label="Row" />
              {Array.from({ length: columns }, (_, c) => (
                <th key={c}>{columnName(c)}</th>
              ))}
              <th className="row-actions" aria-label="Row actions" />
            </tr>
          </thead>
          <tbody>
            {table.rows.map((row, r) => (
              <tr key={r}>
                <td className="row-number">{r + 1}</td>
                {Array.from({ length: columns }, (_, c) => (
                  <td key={c}>
                    <input
                      value={row[c] ?? ''}
                      onChange={(e) => setCell(r, c, e.target.value)}
                      aria-label={`Row ${r + 1}, column ${columnName(c)}`}
                    />
                  </td>
                ))}
                <td className="row-actions">
                  <Tooltip title="Delete row">
                    <Button
                      size="small"
                      type="text"
                      icon={<DeleteOutlined />}
                      onClick={() => deleteRow(r)}
                      aria-label={`Delete row ${r + 1}`}
                    />
                  </Tooltip>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

/** Spreadsheet-style column names: A…Z, AA… */
function columnName(index: number): string {
  let name = '';
  let n = index + 1;
  while (n > 0) {
    const rem = (n - 1) % 26;
    name = String.fromCharCode(65 + rem) + name;
    n = Math.floor((n - 1) / 26);
  }
  return name;
}
