import { describe, expect, test } from 'bun:test';
import {
  DEFAULT_MAX_CONCURRENT_UPLOADS,
  MAX_CONCURRENT_UPLOADS,
  MIN_CONCURRENT_UPLOADS,
  clampUploadConcurrency,
  planUploadStarts,
} from './uploadQueue';

const tasks = (...statuses: string[]) => statuses.map((status, i) => ({ id: `t${i}`, status }));

describe('clampUploadConcurrency', () => {
  test('keeps the limit within 3–5', () => {
    expect(DEFAULT_MAX_CONCURRENT_UPLOADS).toBe(3);
    expect(clampUploadConcurrency(1)).toBe(MIN_CONCURRENT_UPLOADS);
    expect(clampUploadConcurrency(4)).toBe(4);
    expect(clampUploadConcurrency(50)).toBe(MAX_CONCURRENT_UPLOADS);
    expect(clampUploadConcurrency(4.4)).toBe(4);
    expect(clampUploadConcurrency(Number.NaN)).toBe(DEFAULT_MAX_CONCURRENT_UPLOADS);
  });
});

describe('planUploadStarts', () => {
  test('starts queued tasks in order up to the limit', () => {
    const planned = planUploadStarts(tasks('queued', 'queued', 'queued', 'queued', 'queued'), 3);
    expect(planned.map((t) => t.id)).toEqual(['t0', 't1', 't2']);
  });

  test('counts running uploads against the limit', () => {
    const planned = planUploadStarts(tasks('uploading', 'uploading', 'queued', 'queued'), 3);
    expect(planned.map((t) => t.id)).toEqual(['t2']);
    expect(planUploadStarts(tasks('uploading', 'uploading', 'uploading', 'queued'), 3)).toEqual([]);
  });

  test('ignores pending and finished tasks', () => {
    const planned = planUploadStarts(
      tasks('pending', 'success', 'error', 'cancelled', 'queued'),
      3
    );
    expect(planned.map((t) => t.id)).toEqual(['t4']);
  });

  test('a lower limit starts nothing until running uploads drain', () => {
    const running = tasks('uploading', 'uploading', 'uploading', 'uploading', 'queued');
    expect(planUploadStarts(running, 3)).toEqual([]);
    expect(planUploadStarts(running, 5).map((t) => t.id)).toEqual(['t4']);
  });
});
