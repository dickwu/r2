/**
 * Wall-clock ms at the current render, without a ticking subscription.
 * Matches the previous `Date.now()`-during-render behavior: the value only
 * changes when something else re-renders the component.
 *
 * Do not wrap this in useSyncExternalStore. getSnapshot must be Object.is
 * stable between notifications, and `Date.now()` is not, which sends React
 * into a "Maximum update depth exceeded" loop.
 */
export function useRenderNow(): number {
  // Render-time clock, not a ticking subscription. Date.now() is intentionally
  // impure here; a useSyncExternalStore snapshot would change every call and loop.
  // oxlint-disable-next-line react/purity
  return Date.now();
}
