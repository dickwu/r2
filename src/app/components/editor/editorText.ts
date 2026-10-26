import type { Editability } from '@/app/components/editor/editability';
import { errorMessage } from '@/app/components/editor/saveFlow';

/** A file the editor can open, and how. */
export type EditableAs = Extract<Editability, { editable: true }>;

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** What the user is editing, in their words: "Markdown", "CSV", "TypeScript". */
export function documentLabel(editability: EditableAs): string {
  if (editability.mode === 'image') return editability.mime.replace('image/', '').toUpperCase();
  switch (editability.flavor) {
    case 'markdown':
      return 'Markdown';
    case 'json':
      return 'JSON';
    case 'delimited':
      return editability.delimiter === '\t' ? 'TSV' : 'CSV';
  }
  const names: Record<string, string> = {
    plaintext: 'Plain text',
    javascript: 'JavaScript',
    typescript: 'TypeScript',
    html: 'HTML',
    css: 'CSS',
    scss: 'SCSS',
    less: 'Less',
    xml: 'XML',
    yaml: 'YAML',
    ini: 'INI',
    sql: 'SQL',
    shell: 'Shell',
    python: 'Python',
    go: 'Go',
    rust: 'Rust',
    csharp: 'C#',
    cpp: 'C++',
    php: 'PHP',
    dockerfile: 'Dockerfile',
  };
  const id = editability.language;
  return names[id] ?? id.charAt(0).toUpperCase() + id.slice(1);
}

/** The indent a JSON document already uses, so formatting keeps its style. */
export function jsonIndent(text: string): string | number {
  const match = /^([ \t]+)["{[\]}]/m.exec(text);
  if (!match) return 2;
  return match[1].startsWith('\t') ? '\t' : match[1].length;
}

/** Where a JSON document stops parsing, as a message, or null when it parses. */
export function jsonProblem(text: string): string | null {
  if (text.trim() === '') return null;
  try {
    JSON.parse(text);
    return null;
  } catch (error) {
    const message = errorMessage(error);
    const position = /position (\d+)/.exec(message);
    if (!position) return message;
    const offset = Number(position[1]);
    const before = text.slice(0, offset);
    const line = before.split('\n').length;
    const column = offset - before.lastIndexOf('\n');
    return `Invalid JSON at line ${line}, column ${column}`;
  }
}
