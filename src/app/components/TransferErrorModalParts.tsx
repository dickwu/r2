'use client';

import { Fragment, type KeyboardEvent } from 'react';
import {
  KIND_LABEL,
  failurePercent,
  firstLine,
  formatFailureAmount,
  formatFailureClock,
  formatFailureTimestamp,
  isCountKind,
  splitErrorCode,
  type TransferFailure,
} from '@/app/lib/transferFailure';

interface FaultListProps {
  failures: readonly TransferFailure[];
  focusedId: string;
  onSelect: (id: string) => void;
}

/**
 * Every failure on record, newest first; a click or the arrow keys pick the
 * one shown below. One Tab stop: the selected row (roving tabindex).
 */
export function FaultList({ failures, focusedId, onSelect }: FaultListProps) {
  const rows = [...failures].reverse();

  const handleKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const step = event.key === 'ArrowDown' ? 1 : event.key === 'ArrowUp' ? -1 : 0;
    if (step === 0) return;
    event.preventDefault();
    const current = rows.findIndex((failure) => failure.id === focusedId);
    const next = Math.min(rows.length - 1, Math.max(0, current + step));
    onSelect(rows[next].id);
    event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="option"]')[next]?.focus();
  };

  return (
    <div className="fault-list" role="listbox" aria-label="Failures" onKeyDown={handleKeyDown}>
      {rows.map((failure) => {
        const selected = failure.id === focusedId;
        return (
          <button
            key={failure.id}
            type="button"
            role="option"
            aria-selected={selected}
            tabIndex={selected ? 0 : -1}
            className={selected ? 'fault-row selected' : 'fault-row'}
            onClick={() => onSelect(failure.id)}
          >
            <span className="fault-row-kind">{KIND_LABEL[failure.kind]}</span>
            <span className="fault-row-name">{failure.name}</span>
            <span className="fault-row-msg">{firstLine(failure.message)}</span>
            <span className="fault-row-time">{formatFailureClock(failure.occurredAt)}</span>
          </button>
        );
      })}
    </div>
  );
}

interface FaultLineProps {
  failure: TransferFailure;
  /** Bytes for transfers, plain counts for delete/rename. */
  format: (n: number) => string;
}

/**
 * The signature: the lane bar the person was watching, frozen where it died.
 * Only for a transfer that stopped part-way — a delete or rename batch runs to
 * the end and reports which items failed, so "stopped at" would be untrue.
 */
export function FaultLine({ failure, format }: FaultLineProps) {
  const pct = failurePercent(failure.progress);
  const amount = formatFailureAmount(failure, format);
  if (pct === null || amount === null || isCountKind(failure.kind)) return null;

  return (
    <div className="fault-line" role="img" aria-label={`Stopped at ${pct}%`}>
      <div className="fault-line-track">
        <div className="fault-line-fill" style={{ width: `${pct}%` }} />
      </div>
      <div className="fault-line-caption">
        <span>{amount}</span>
        <span className="fault-line-pct">
          <em>stopped at</em>
          {pct}%
        </span>
      </div>
    </div>
  );
}

/** The error itself: the provider code as a chip, then the text verbatim and selectable. */
export function FaultMessage({ message }: { message: string }) {
  const { code, text } = splitErrorCode(message);

  return (
    <div className="fault-msg">
      {code && <span className="fault-code">{code}</span>}
      <pre className="fault-text">{text}</pre>
    </div>
  );
}

interface Fact {
  label: string;
  value?: string;
  mono?: boolean;
  title?: string;
}

/** Where and when, in a fixed order, showing only the facts this record has. */
export function FaultFacts({ failure }: { failure: TransferFailure }) {
  const facts: Fact[] = [
    { label: 'Kind', value: KIND_LABEL[failure.kind] },
    { label: 'Account', value: failure.account },
    { label: 'Bucket', value: failure.bucket },
    { label: 'Key', value: failure.key, mono: true },
    { label: 'Stage', value: failure.stage },
    {
      label: 'When',
      value: formatFailureClock(failure.occurredAt),
      title: formatFailureTimestamp(failure.occurredAt),
    },
  ];

  return (
    <dl className="fault-facts">
      {facts
        .filter((fact) => fact.value !== undefined && fact.value.trim() !== '')
        .map((fact) => (
          <Fragment key={fact.label}>
            <dt>{fact.label}</dt>
            <dd className={fact.mono ? 'mono' : undefined} title={fact.title}>
              {fact.value}
            </dd>
          </Fragment>
        ))}
    </dl>
  );
}
