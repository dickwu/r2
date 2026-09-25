import { create } from 'zustand';
import { logSession } from '@/app/lib/diagnostics/sessionLog';

/** What the report starts with — e.g. a failed transfer's details. */
export interface ReportPrefill {
  title?: string;
  description?: string;
}

/**
 * Prefilled text cut to a field's limit — never through a surrogate pair,
 * whose lone half makes encodeURIComponent throw when the issue link is built.
 */
export const clampPrefillText = (value: string | undefined, max: number): string => {
  if (!value) return '';
  if (value.length <= max) return value;
  const cut = value.slice(0, max);
  return /[\uD800-\uDBFF]$/.test(cut) ? cut.slice(0, -1) : cut;
};

/**
 * A prefill that arrives while the report is already open: the text goes below
 * what the reporter has written — never over it — and the result is cut to the limit.
 */
export const appendPrefillText = (
  current: string,
  incoming: string | undefined,
  max: number
): string => {
  if (!incoming) return current;
  if (current.trim() === '') return clampPrefillText(incoming, max);
  return clampPrefillText(`${current}\n\n${incoming}`, max);
};

/**
 * Whether the "Report a problem" dialog is showing, and what it starts with.
 * A store rather than local state because several places open it — the
 * status-bar button, the command palette and the transfer failure modal —
 * while the dialog itself is mounted once, next to the palette.
 */
interface ReportStore {
  isOpen: boolean;
  /** Seeds the dialog's title and description; null for a blank report. */
  prefill: ReportPrefill | null;
  open: (prefill?: ReportPrefill) => void;
  close: () => void;
}

export const useReportStore = create<ReportStore>((set) => ({
  isOpen: false,
  prefill: null,
  open: (prefill) => {
    // Marks the moment the person decided something was wrong — the point every
    // line in the attached log is read as a countdown to. Logged here, on the
    // click, rather than in the dialog's mount effect, which StrictMode runs twice.
    logSession('app', 'info', 'Report a problem opened');
    set({ isOpen: true, prefill: prefill ?? null });
  },
  close: () => set({ isOpen: false, prefill: null }),
}));
