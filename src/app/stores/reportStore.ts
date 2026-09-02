import { create } from 'zustand';
import { logSession } from '@/app/lib/diagnostics/sessionLog';

/**
 * Whether the "Report a problem" dialog is showing. A store rather than local
 * state because two places open it — the status-bar button and the command
 * palette — while the dialog itself is mounted once, next to the palette.
 */
interface ReportStore {
  isOpen: boolean;
  open: () => void;
  close: () => void;
}

export const useReportStore = create<ReportStore>((set) => ({
  isOpen: false,
  open: () => {
    // Marks the moment the person decided something was wrong — the point every
    // line in the attached log is read as a countdown to. Logged here, on the
    // click, rather than in the dialog's mount effect, which StrictMode runs twice.
    logSession('app', 'info', 'Report a problem opened');
    set({ isOpen: true });
  },
  close: () => set({ isOpen: false }),
}));
