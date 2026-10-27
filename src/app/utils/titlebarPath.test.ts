import { describe, expect, test } from 'bun:test';
import { titlebarPathSegments, titlebarPathTitle } from './titlebarPath';

describe('titlebarPathSegments', () => {
  test('bucket root has no segments', () => {
    expect(titlebarPathSegments('')).toEqual([]);
    expect(titlebarPathSegments('/')).toEqual([]);
    expect(titlebarPathSegments(null)).toEqual([]);
    expect(titlebarPathSegments(undefined)).toEqual([]);
  });

  test('splits a folder path and drops empty segments', () => {
    expect(titlebarPathSegments('r2-connector-test/')).toEqual(['r2-connector-test']);
    expect(titlebarPathSegments('photos/2024/')).toEqual(['photos', '2024']);
    expect(titlebarPathSegments('/a//b')).toEqual(['a', 'b']);
  });
});

describe('titlebarPathTitle', () => {
  test('keeps the bucket and folder apart', () => {
    expect(titlebarPathTitle('gfit', 'r2-connector-test/')).toBe('gfit/r2-connector-test/');
    expect(titlebarPathTitle('gfit', 'photos/2024/')).toBe('gfit/photos/2024/');
  });

  test('bucket root', () => {
    expect(titlebarPathTitle('gfit', '')).toBe('gfit/');
    expect(titlebarPathTitle('gfit', '/')).toBe('gfit/');
  });
});
