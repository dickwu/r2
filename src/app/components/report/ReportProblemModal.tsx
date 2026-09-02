'use client';

import { useEffect, useState, type FormEvent } from 'react';
import { App } from 'antd';
import { BugOutlined } from '@ant-design/icons';
import { openUrl } from '@tauri-apps/plugin-opener';
import Modal from '@/app/components/ui/Modal';
import SessionLogPanel from '@/app/components/report/SessionLogPanel';
import { recentSessionLog, type SessionLogSnapshot } from '@/app/lib/diagnostics/sessionLog';
import { buildIssueUrl, type IssueUrlResult } from '@/app/lib/report/issueUrl';
import {
  buildReportInfo,
  fallbackReportInfo,
  type AppReportInfo,
  type ReportContext,
} from '@/app/lib/report/reportInfo';
import { useAccountStore } from '@/app/stores/accountStore';
import { useReportStore } from '@/app/stores/reportStore';
import { useThemeStore } from '@/app/stores/themeStore';

const TITLE_MAX = 120;
const DESCRIPTION_MAX = 2000;
const FORM_ID = 'report-problem-form';

const isTauri = (): boolean => typeof window !== 'undefined' && '__TAURI__' in window;

/** Hand the prefilled issue to the system browser; plain-browser dev has no opener plugin. */
const openInBrowser = async (url: string): Promise<void> => {
  if (isTauri()) {
    await openUrl(url);
    return;
  }
  window.open(url, '_blank', 'noopener');
};

const copyToClipboard = async (text: string): Promise<boolean> => {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
};

/** Tell the reporter what the link could not carry, and rescue a cut description. */
const explainCuts = async (
  result: IssueUrlResult,
  description: string,
  warn: (text: string) => void
): Promise<void> => {
  if (result.descriptionTrimmed) {
    const copied = await copyToClipboard(description);
    warn(
      copied
        ? 'The description was cut to fit the link — the full text is on your clipboard, paste it on GitHub.'
        : 'The description was cut to fit the link — add the rest on GitHub.'
    );
  } else if (result.droppedLogLines > 0) {
    warn(`${result.droppedLogLines} older log lines were left out to keep the link short.`);
  }
};

const currentContext = (): ReportContext => ({
  provider: useAccountStore.getState().currentConfig?.provider ?? 'none',
  theme: useThemeStore.getState().theme,
});

/**
 * The report dialog is evidence-first: the runtime facts and the app's own
 * last few minutes sit on top, exactly as they will be sent, and the reporter
 * only adds the things the app cannot know — what they were doing and what
 * went wrong. Nothing is collected that isn't shown.
 */
function ReportProblemDialog() {
  const { message } = App.useApp();
  const close = useReportStore((s) => s.close);
  const [title, setTitle] = useState('');
  const [description, setDescription] = useState('');
  const [includeLog, setIncludeLog] = useState(true);
  const [info, setInfo] = useState<AppReportInfo | null>(null);
  const [log, setLog] = useState<SessionLogSnapshot | null>(null);
  const [opening, setOpening] = useState(false);

  // Evidence is captured the moment the dialog opens, not when the reporter
  // finishes typing. Snapshotting is pure, so StrictMode's double effect is harmless.
  useEffect(() => {
    let cancelled = false;
    setLog(recentSessionLog());
    buildReportInfo(currentContext()).then((next) => {
      if (!cancelled) setInfo(next);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  const canSubmit = title.trim() !== '' && description.trim() !== '' && !opening;

  const handleSubmit = async (event: FormEvent) => {
    event.preventDefault();
    if (!canSubmit) return;
    setOpening(true);

    const result = buildIssueUrl({
      title,
      description,
      info: info ?? fallbackReportInfo(currentContext()),
      log: includeLog ? log : null,
    });

    try {
      await openInBrowser(result.url);
    } catch (error) {
      console.error('[ReportProblem] could not open the browser:', error);
      message.error('Could not open your browser. Please try again.');
      setOpening(false);
      return;
    }

    await explainCuts(result, description, (text) => message.warning(text, 8));
    message.success('Issue draft opened on GitHub — review it there and press Submit');
    close();
  };

  const meta = info
    ? [
        info.appVersion ? `v${info.appVersion}` : '',
        info.os,
        `provider: ${info.provider}`,
        info.window,
      ].filter((part) => part !== '')
    : ['Collecting app info…'];

  return (
    <Modal
      open
      onClose={close}
      title="Report a problem"
      subtitle="Opens a GitHub issue draft pre-filled with the details below. Review it there — nothing is filed until you press Submit."
      icon={<BugOutlined style={{ fontSize: 18 }} />}
      width={600}
      footer={
        <>
          <span style={{ marginRight: 'auto', fontSize: 11.5, color: 'var(--text-subtle)' }}>
            Needs a GitHub account
          </span>
          <button type="button" className="btn" onClick={close}>
            Cancel
          </button>
          <button type="submit" form={FORM_ID} className="btn btn-primary" disabled={!canSubmit}>
            {opening ? 'Opening…' : 'Open on GitHub ↗'}
          </button>
        </>
      }
    >
      <form id={FORM_ID} onSubmit={handleSubmit}>
        <div className="report-meta">
          {meta.map((part) => (
            <span key={part}>{part}</span>
          ))}
        </div>

        <div className="field">
          <label className="field-label field-required" htmlFor="report-title">
            Title
          </label>
          <input
            id="report-title"
            className="input"
            value={title}
            maxLength={TITLE_MAX}
            placeholder="Short summary, e.g. Upload stalls at 99%"
            onChange={(e) => setTitle(e.target.value)}
            autoFocus
          />
        </div>

        <div className="field">
          <label className="field-label field-required" htmlFor="report-description">
            What went wrong?
          </label>
          <textarea
            id="report-description"
            className="textarea report-description"
            rows={5}
            value={description}
            maxLength={DESCRIPTION_MAX}
            placeholder="What you were doing, what you expected, and what happened instead"
            onChange={(e) => setDescription(e.target.value)}
          />
          <span className="field-hint">
            {description.length} / {DESCRIPTION_MAX}
          </span>
        </div>

        <button
          type="button"
          className={['toggle-row', 'report-toggle', includeLog && 'on'].filter(Boolean).join(' ')}
          onClick={() => setIncludeLog((on) => !on)}
          aria-pressed={includeLog}
        >
          <span className="option-row-text">
            <strong>Include recent app log</strong>
            <span>
              Warnings and errors from the last 5 minutes. Access keys and secrets are redacted;
              file and folder names are not — review every line below before you send it.
            </span>
          </span>
          <span className="toggle-switch">
            <span className="toggle-knob" />
          </span>
        </button>

        {includeLog && log && <SessionLogPanel log={log} />}
      </form>
    </Modal>
  );
}

export default function ReportProblemModal() {
  const isOpen = useReportStore((s) => s.isOpen);

  // Mounted only while open, so every field, snapshot and app-info read starts
  // fresh on the next report — nothing stale survives a cancel.
  if (!isOpen) return null;
  return <ReportProblemDialog />;
}
