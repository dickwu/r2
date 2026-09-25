import { create } from 'zustand';
import type { TransferFailure } from '@/app/lib/transferFailure';

/**
 * Every transfer failure this session has seen, and whether the modal that
 * explains them is showing. Progress rows, the dock and the sync pill never
 * render an error's text: stores report failures here, and those surfaces
 * offer one button that shows the record again. Records outlive the modal
 * (close keeps them) so a row can reopen its own failure later.
 */
export interface TransferErrorStore {
  isOpen: boolean;
  /** Newest last. */
  failures: TransferFailure[];
  focusedId: string | null;
  /**
   * From stores/hooks when a task ENTERS a failed state. Upserts by id and
   * always records the new occurredAt: failing again is a new event, so "When"
   * moves with it. Opens the modal only when the id is new or its message
   * changed — a background sync failing every minute with the same text must
   * not keep reopening it. An already-open modal stays on the failure the
   * person is reading.
   */
  report: (failure: TransferFailure) => void;
  /**
   * From a row's "Show error" button. Upserts by id, keeping the original
   * occurredAt when the message is unchanged (a click is not a new failure),
   * focuses it and always opens.
   */
  show: (failure: TransferFailure) => void;
  /**
   * Focuses `id` when it exists, else keeps the current focus, else the newest
   * record (the first row of the newest-first list); opens. With no records
   * there is nothing to show, and it stays closed.
   */
  open: (id?: string) => void;
  select: (id: string) => void;
  /** Hides; records stay so rows can reopen them. */
  close: () => void;
  /** Empties records and closes. */
  clear: () => void;
  /**
   * A task left its failed state — retried, resumed or cancelled — so its
   * record is over. Failing again later is a new event and opens the modal
   * again; without this, the same message would read as a repeat.
   */
  forget: (id: string) => void;
}

/** Past this the oldest records drop, so a mass failure cannot grow the list without bound. */
export const MAX_FAILURES = 100;

/** Selector: the focused failure or the last one. */
export const selectFocusedFailure = (s: TransferErrorStore): TransferFailure | null =>
  s.failures.find((f) => f.id === s.focusedId) ?? s.failures[s.failures.length - 1] ?? null;

const hasRecord = (
  failures: readonly TransferFailure[],
  id: string | null | undefined
): id is string => id != null && failures.some((f) => f.id === id);

/**
 * Insert or replace by id without touching the old array. A record whose
 * message is unchanged is replaced where it sits; a new failure, or one that
 * failed again differently, becomes the newest record.
 */
const upsert = (
  failures: readonly TransferFailure[],
  failure: TransferFailure
): { failures: TransferFailure[]; changed: boolean } => {
  const existing = failures.find((f) => f.id === failure.id);
  if (existing && existing.message === failure.message) {
    return { failures: failures.map((f) => (f.id === failure.id ? failure : f)), changed: false };
  }
  const rest = failures.filter((f) => f.id !== failure.id);
  return { failures: [...rest, failure], changed: true };
};

/** A click on "Show error" is not a new failure: an unchanged record keeps when it happened. */
const withOriginalTime = (
  failures: readonly TransferFailure[],
  failure: TransferFailure
): TransferFailure => {
  const existing = failures.find((f) => f.id === failure.id);
  return existing && existing.message === failure.message
    ? { ...failure, occurredAt: existing.occurredAt }
    : failure;
};

/** Drop the oldest records past the cap — never `keepId`, the one on screen. */
const trimOldest = (failures: TransferFailure[], keepId: string | null): TransferFailure[] => {
  const excess = failures.length - MAX_FAILURES;
  if (excess <= 0) return failures;
  const dropped = new Set(
    failures
      .filter((f) => f.id !== keepId)
      .slice(0, excess)
      .map((f) => f.id)
  );
  return failures.filter((f) => !dropped.has(f.id));
};

export const useTransferErrorStore = create<TransferErrorStore>((set) => ({
  isOpen: false,
  failures: [],
  focusedId: null,

  report: (failure) =>
    set((state) => {
      const { failures, changed } = upsert(state.failures, failure);
      if (!changed) return { failures };
      const reading = state.isOpen ? (selectFocusedFailure(state)?.id ?? null) : null;
      const focusedId = reading ?? failure.id;
      return { failures: trimOldest(failures, focusedId), isOpen: true, focusedId };
    }),

  show: (failure) =>
    set((state) => ({
      failures: trimOldest(
        upsert(state.failures, withOriginalTime(state.failures, failure)).failures,
        failure.id
      ),
      isOpen: true,
      focusedId: failure.id,
    })),

  open: (id) =>
    set((state) => {
      const { failures } = state;
      if (failures.length === 0) return state;
      const focusedId = hasRecord(failures, id)
        ? id
        : hasRecord(failures, state.focusedId)
          ? state.focusedId
          : failures[failures.length - 1].id;
      return { isOpen: true, focusedId };
    }),

  select: (id) => set((state) => (hasRecord(state.failures, id) ? { focusedId: id } : state)),

  close: () => set({ isOpen: false }),

  clear: () => set({ isOpen: false, failures: [], focusedId: null }),

  forget: (id) =>
    set((state) => {
      if (!hasRecord(state.failures, id)) return state;
      const failures = state.failures.filter((f) => f.id !== id);
      const focusedId =
        state.focusedId === id ? (failures[failures.length - 1]?.id ?? null) : state.focusedId;
      return { failures, focusedId, isOpen: state.isOpen && failures.length > 0 };
    }),
}));
