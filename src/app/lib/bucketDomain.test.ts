import { describe, expect, test } from 'bun:test';
import { publicAfterDomainEdit } from './bucketDomain';

describe('publicAfterDomainEdit', () => {
  test('a first domain on a private bucket makes it public', () => {
    expect(publicAfterDomainEdit('', 'cdn.example.com', false)).toBe(true);
    expect(publicAfterDomainEdit(undefined, 'c', false)).toBe(true);
    expect(publicAfterDomainEdit(null, 'c', undefined)).toBe(true);
  });

  test('a public bucket stays public whatever the domain becomes', () => {
    expect(publicAfterDomainEdit('cdn.example.com', '', true)).toBe(true);
    expect(publicAfterDomainEdit('', 'cdn.example.com', true)).toBe(true);
  });

  test('editing a domain keeps an explicit private choice', () => {
    expect(publicAfterDomainEdit('cdn.example.com', 'cdn2.example.com', false)).toBe(false);
    expect(publicAfterDomainEdit('cdn.example.com', '', false)).toBe(false);
  });

  test('blank input does not make a bucket public', () => {
    expect(publicAfterDomainEdit('', '   ', false)).toBe(false);
    expect(publicAfterDomainEdit('', '', false)).toBe(false);
  });
});
