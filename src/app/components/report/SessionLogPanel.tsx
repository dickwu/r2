'use client';

import { useTranslation } from 'react-i18next';
import { useState } from 'react';
import { DownOutlined, RightOutlined } from '@ant-design/icons';
import type { SessionLogLevel, SessionLogSnapshot } from '@/app/lib/diagnostics/sessionLog';
import { beforeReport } from '@/app/lib/report/issueUrl';

/**
 * Left-edge colour per severity. Info is deliberately invisible: a log where
 * every row is striped is a log where no row stands out.
 */
const LEVEL_RAIL: Record<SessionLogLevel, string> = {
  error: '#e5484d',
  warn: '#e0a300',
  info: 'transparent',
};

/**
 * What the app was doing in the minutes before this report, shown before it
 * leaves the app. The dialog's rule is that nothing is sent the reporter has
 * not seen, and this is the one part they did not type — so it sits in the
 * same evidence box as the app info, unfolded whenever it has warnings or errors.
 */
export default function SessionLogPanel({ log }: { log: SessionLogSnapshot }) {
  const { t } = useTranslation();
  // Open by default whenever there is something to review: a warning or error
  // line can carry an object key or a path, and the reporter should see it
  // before it goes into a public issue. Our own info markers stay folded.
  const [expanded, setExpanded] = useState(() =>
    log.entries.some((entry) => entry.level !== 'info')
  );
  const errors = log.entries.filter((entry) => entry.level === 'error').length;
  const lines = log.entries.length;

  return (
    <div className="report-log">
      <button
        type="button"
        className="report-log-head"
        onClick={() => setExpanded((open) => !open)}
        aria-expanded={expanded}
      >
        {expanded ? (
          <DownOutlined style={{ fontSize: 10 }} />
        ) : (
          <RightOutlined style={{ fontSize: 10 }} />
        )}
        <span>{t('report.log.title', { minutes: log.windowMinutes })}</span>
        <span className="report-log-count">
          {lines === 0 ? t('report.log.nothing') : t('report.log.lines', { count: lines })}
        </span>
        {errors > 0 && (
          // The one number worth a colour here: it tells the reporter the app
          // noticed something too, before they have read a single line.
          <span className="report-log-errors">{t('report.log.errors', { count: errors })}</span>
        )}
      </button>

      {expanded && (
        <div className="report-log-body">
          {log.truncated && <div className="report-log-note">{t('report.log.truncated')}</div>}
          {log.entries.map((entry, index) => (
            <div
              key={index}
              className="report-log-line"
              style={{ borderLeftColor: LEVEL_RAIL[entry.level] }}
            >
              <span className="report-log-t">{beforeReport(entry.ts, log.capturedTs)}</span>
              <span className={`report-log-msg ${entry.level}`}>{entry.msg}</span>
            </div>
          ))}
          {lines === 0 && (
            <div className="report-log-note">
              {t('report.log.empty', { minutes: log.windowMinutes })}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
