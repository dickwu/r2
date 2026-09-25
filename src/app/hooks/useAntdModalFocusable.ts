'use client';

import { useMemo } from 'react';
import { useReportStore } from '@/app/stores/reportStore';
import { useTransferErrorStore } from '@/app/stores/transferErrorStore';

/**
 * `focusable` for an antd Modal. antd pulls focus back inside its modal on
 * every focusin, which would leave the keyboard under the transfer failure
 * modal or the report dialog whenever one opens above it (they sit at
 * ABOVE_ANTD_MODALS_Z_INDEX). The trap lifts while either is open and returns
 * when both have closed.
 */
export function useAntdModalFocusable(): { trap: boolean } {
  const failureOpen = useTransferErrorStore((s) => s.isOpen);
  const reportOpen = useReportStore((s) => s.isOpen);
  const trap = !(failureOpen || reportOpen);
  return useMemo(() => ({ trap }), [trap]);
}
