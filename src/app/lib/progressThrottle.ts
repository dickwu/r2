/**
 * Shared throttled batcher for high-frequency Tauri progress events.
 *
 * The first event for a task flushes immediately; events arriving inside the
 * throttle window are coalesced (latest wins per task) and flushed together on
 * a single timer. This keeps store updates — and therefore React re-renders —
 * bounded no matter how many tasks emit concurrently.
 */
export interface ProgressBatcher<E> {
  push: (key: string, event: E) => void;
}

export function createProgressBatcher<E>(
  throttleMs: number,
  flush: (updates: Map<string, E>) => void
): ProgressBatcher<E> {
  const lastUpdate = new Map<string, number>();
  const pending = new Map<string, E>();
  let timer: ReturnType<typeof setTimeout> | null = null;

  return {
    push(key, event) {
      const now = Date.now();
      const last = lastUpdate.get(key) || 0;
      pending.set(key, event);

      if (now - last < throttleMs) {
        if (!timer) {
          timer = setTimeout(() => {
            timer = null;
            const updates = new Map(pending);
            pending.clear();
            if (updates.size === 0) return;
            const stamp = Date.now();
            for (const key of updates.keys()) lastUpdate.set(key, stamp);
            flush(updates);
          }, throttleMs);
        }
        return;
      }

      lastUpdate.set(key, now);
      pending.delete(key);
      flush(new Map([[key, event]]));
    },
  };
}

/**
 * Exponential smoothing for display speed. Raw per-event rates jitter; blending
 * each new sample into the previous reading keeps the readout steady without
 * hiding real trends. A zero sample means idle/paused and passes through.
 */
export function smoothSpeed(previous: number, next: number): number {
  if (next <= 0) return 0;
  if (previous <= 0) return next;
  return previous * 0.65 + next * 0.35;
}

/** Seconds remaining at the current rate, or 0 when it cannot be estimated. */
export function etaSeconds(totalBytes: number, transferredBytes: number, speed: number): number {
  if (speed <= 0 || totalBytes <= 0) return 0;
  const remaining = totalBytes - transferredBytes;
  if (remaining <= 0) return 0;
  return remaining / speed;
}
