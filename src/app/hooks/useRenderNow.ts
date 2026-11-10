import { useSyncExternalStore } from 'react';

const subscribe = () => () => {};
const getNow = () => Date.now();

/**
 * Wall-clock ms at the current render, without a ticking subscription.
 * Matches the previous `Date.now()`-during-render behavior: the value only
 * changes when something else re-renders the component.
 */
export function useRenderNow(): number {
  return useSyncExternalStore(subscribe, getNow, getNow);
}
