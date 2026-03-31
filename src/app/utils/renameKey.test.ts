import { describe, expect, test } from 'bun:test';
import { renameKey } from './renameKey';

const fixedDate = new Date(2026, 2, 31, 15, 30, 12, 847); // 2026-03-31 15:30:12.847

describe('renameKey', () => {
  test('overwrite mode returns fileName unchanged', () => {
    expect(renameKey('report.jpg', 'overwrite', fixedDate)).toBe('report.jpg');
  });

  test('empty fileName returns empty string', () => {
    expect(renameKey('', 'auto-rename', fixedDate)).toBe('');
  });

  test('normal file gets timestamp before extension', () => {
    expect(renameKey('report.jpg', 'auto-rename', fixedDate)).toBe(
      'report-20260331-153012-847.jpg'
    );
  });

  test('file with no extension gets timestamp appended', () => {
    expect(renameKey('Makefile', 'auto-rename', fixedDate)).toBe('Makefile-20260331-153012-847');
  });

  test('dotfile gets timestamp appended', () => {
    expect(renameKey('.gitignore', 'auto-rename', fixedDate)).toBe(
      '.gitignore-20260331-153012-847'
    );
  });

  test('dotfile with extension gets timestamp before extension', () => {
    expect(renameKey('.env.local', 'auto-rename', fixedDate)).toBe(
      '.env-20260331-153012-847.local'
    );
  });

  test('compound extension .tar.gz stays together', () => {
    expect(renameKey('archive.tar.gz', 'auto-rename', fixedDate)).toBe(
      'archive-20260331-153012-847.tar.gz'
    );
  });

  test('compound extension .tar.bz2 stays together', () => {
    expect(renameKey('backup.tar.bz2', 'auto-rename', fixedDate)).toBe(
      'backup-20260331-153012-847.tar.bz2'
    );
  });

  test('compound extension .tar.xz stays together', () => {
    expect(renameKey('data.tar.xz', 'auto-rename', fixedDate)).toBe(
      'data-20260331-153012-847.tar.xz'
    );
  });

  test('relative path preserves directory prefix', () => {
    expect(renameKey('images/photo.jpg', 'auto-rename', fixedDate)).toBe(
      'images/photo-20260331-153012-847.jpg'
    );
  });

  test('deep relative path preserves full directory', () => {
    expect(renameKey('a/b/c/file.txt', 'auto-rename', fixedDate)).toBe(
      'a/b/c/file-20260331-153012-847.txt'
    );
  });

  test('multiple dots in filename splits at last dot', () => {
    expect(renameKey('my.file.name.txt', 'auto-rename', fixedDate)).toBe(
      'my.file.name-20260331-153012-847.txt'
    );
  });

  test('relative path with no extension', () => {
    expect(renameKey('scripts/build', 'auto-rename', fixedDate)).toBe(
      'scripts/build-20260331-153012-847'
    );
  });
});
