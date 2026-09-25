'use client';

import { useCallback, useEffect, useRef, useState, type RefObject } from 'react';
import { App } from 'antd';
import { BugOutlined, CheckOutlined, CloseCircleFilled, CopyOutlined } from '@ant-design/icons';
import Modal, { ABOVE_ANTD_MODALS_Z_INDEX } from '@/app/components/ui/Modal';
import {
  FaultFacts,
  FaultLine,
  FaultList,
  FaultMessage,
} from '@/app/components/TransferErrorModalParts';
import { redactSecrets } from '@/app/lib/diagnostics/redact';
import {
  failureTitle,
  formatCount,
  formatFailureDetails,
  isCountKind,
  singleFailureTitle,
  type TransferFailure,
  type TransferFailureKind,
} from '@/app/lib/transferFailure';
import { useReportStore } from '@/app/stores/reportStore';
import { selectFocusedFailure, useTransferErrorStore } from '@/app/stores/transferErrorStore';
import { formatBytes } from '@/app/utils/formatBytes';

const COPIED_FLASH_MS = 1600;

/** Bytes for transfers; plain counts for delete and rename, which work in items. */
const amountFormat = (kind: TransferFailureKind): ((n: number) => string) =>
  isCountKind(kind) ? formatCount : formatBytes;

const subtitleFor = (failures: readonly TransferFailure[], focused: TransferFailure): string => {
  if (failures.length > 1) return 'Select one to see why.';
  return focused.bucket ? `${focused.name} · ${focused.bucket}` : focused.name;
};

/** Which failure's details were just copied — "Copied" shows on that one only, briefly. */
function useCopiedFlash(): [string | null, (id: string) => void] {
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current);
    },
    []
  );

  const flash = useCallback((id: string) => {
    if (timer.current) clearTimeout(timer.current);
    setCopiedId(id);
    timer.current = setTimeout(() => setCopiedId(null), COPIED_FLASH_MS);
  }, []);

  return [copiedId, flash];
}

/**
 * Focus moves to `target` when the modal opens — off the page behind the
 * backdrop, where keystrokes would otherwise keep landing, and onto the body
 * rather than a button, so a stray Space or Enter mid-typing cannot dismiss a
 * failure unread — and back to where it was when the modal closes, unless
 * something else took it meanwhile (the report dialog this modal hands off to
 * focuses its own title field).
 */
function useDialogFocus(target: RefObject<HTMLElement | null>): void {
  // Read on the first render, before this dialog commits and moves focus.
  const [previous] = useState(() =>
    typeof document !== 'undefined' && document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null
  );

  useEffect(() => {
    target.current?.focus();
    return () => {
      const current = document.activeElement;
      if (current === null || current === document.body) previous?.focus();
    };
  }, [target, previous]);
}

/**
 * Why a transfer stopped. Progress rows only say "Failed"; the error text
 * lives here — the provider's code, its message verbatim, and where the
 * transfer was when it died.
 */
function TransferErrorDialog() {
  const { message } = App.useApp();
  const failures = useTransferErrorStore((s) => s.failures);
  const focused = useTransferErrorStore(selectFocusedFailure);
  const select = useTransferErrorStore((s) => s.select);
  const close = useTransferErrorStore((s) => s.close);
  const clear = useTransferErrorStore((s) => s.clear);
  const [copiedId, flashCopied] = useCopiedFlash();
  const body = useRef<HTMLDivElement>(null);
  useDialogFocus(body);

  // The store opens only with a record to show; this narrows the type.
  if (!focused) return null;

  const format = amountFormat(focused.kind);
  const copied = copiedId === focused.id;

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(formatFailureDetails(focused, format));
      flashCopied(focused.id);
    } catch {
      message.error('Could not copy — select the text instead');
    }
  };

  const handleReport = () => {
    // The report becomes a public GitHub issue, so credential-shaped text (key
    // IDs, account IDs in endpoint hosts) is scrubbed from both fields the way
    // the session log scrubs it. The clipboard copy stays verbatim.
    useReportStore.getState().open({
      title: redactSecrets(`${singleFailureTitle(focused)}: ${focused.name}`),
      description: redactSecrets(formatFailureDetails(focused, format)),
    });
    close();
  };

  return (
    <Modal
      open
      onClose={close}
      title={failureTitle(failures)}
      subtitle={subtitleFor(failures, focused)}
      icon={
        <span className="fault-icon">
          <CloseCircleFilled />
        </span>
      }
      width={600}
      zIndex={ABOVE_ANTD_MODALS_Z_INDEX}
      footer={
        <>
          <span className="left" style={{ display: 'flex', gap: 4 }}>
            <button type="button" className="btn btn-ghost" onClick={handleCopy}>
              {copied ? <CheckOutlined /> : <CopyOutlined />}
              {copied ? 'Copied' : 'Copy details'}
            </button>
            <button type="button" className="btn btn-ghost" onClick={handleReport}>
              <BugOutlined />
              Report a problem
            </button>
            {failures.length > 1 && (
              <button type="button" className="btn btn-ghost" onClick={clear}>
                Clear all
              </button>
            )}
          </span>
          <button type="button" className="btn btn-primary" onClick={close}>
            Close
          </button>
        </>
      }
    >
      <div ref={body} tabIndex={-1} className="fault-body">
        {failures.length > 1 && (
          <FaultList failures={failures} focusedId={focused.id} onSelect={select} />
        )}
        <FaultLine failure={focused} format={format} />
        <FaultMessage message={focused.message} />
        <FaultFacts failure={focused} />
      </div>
    </Modal>
  );
}

export default function TransferErrorModal() {
  const isOpen = useTransferErrorStore((s) => s.isOpen);

  // Mounted only while open (the ReportProblemModal pattern), so the copy
  // flash and its timer never outlive the modal.
  if (!isOpen) return null;
  return <TransferErrorDialog />;
}
