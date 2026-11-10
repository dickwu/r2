'use client';

import { t as i18nT } from '@/i18n';
import { useTranslation } from 'react-i18next';
import { useEffect, useState, type FormEvent } from 'react';
import { App } from 'antd';
import { BugOutlined } from '@ant-design/icons';
import { openUrl } from '@tauri-apps/plugin-opener';
import Modal, { ABOVE_ANTD_MODALS_Z_INDEX } from '@/app/components/ui/Modal';
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
import { appendPrefillText, clampPrefillText, useReportStore } from '@/app/stores/reportStore';
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
    warn(copied ? i18nT('report.cutCopied') : i18nT('report.cutAddRest'));
  } else if (result.droppedLogLines > 0) {
    warn(i18nT('report.droppedLines', { count: result.droppedLogLines }));
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
  const { t } = useTranslation();
  const close = useReportStore((s) => s.close);
  const prefill = useReportStore((s) => s.prefill);
  const [title, setTitle] = useState(() => clampPrefillText(prefill?.title, TITLE_MAX));
  const [description, setDescription] = useState(() =>
    clampPrefillText(prefill?.description, DESCRIPTION_MAX)
  );
  const [includeLog, setIncludeLog] = useState(true);
  const [info, setInfo] = useState<AppReportInfo | null>(null);
  const [log] = useState<SessionLogSnapshot | null>(() => recentSessionLog());
  const [opening, setOpening] = useState(false);

  // A prefill that arrives while the dialog is already open (Report a problem
  // on a failed transfer): a blank title takes it, and the details land below
  // whatever the reporter has written — a draft is never replaced.
  const [appliedPrefill, setAppliedPrefill] = useState(prefill);
  if (prefill && prefill !== appliedPrefill) {
    setAppliedPrefill(prefill);
    setTitle((current) =>
      current.trim() === '' ? clampPrefillText(prefill.title, TITLE_MAX) : current
    );
    setDescription((current) => appendPrefillText(current, prefill.description, DESCRIPTION_MAX));
  }

  // Evidence is captured the moment the dialog opens, not when the reporter
  // finishes typing. Snapshotting is pure, so StrictMode's double effect is harmless.
  useEffect(() => {
    let cancelled = false;
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
      message.error(t('report.browserFailed'));
      setOpening(false);
      return;
    }

    await explainCuts(result, description, (text) => message.warning(text, 8));
    message.success(t('report.opened'));
    close();
  };

  const meta = info
    ? [
        info.appVersion ? `v${info.appVersion}` : '',
        info.os,
        t('report.provider', { provider: info.provider }),
        info.window,
      ].filter((part) => part !== '')
    : [t('report.collecting')];

  return (
    <Modal
      open
      onClose={close}
      title={t('report.trigger')}
      subtitle={t('report.subtitle')}
      icon={<BugOutlined style={{ fontSize: 18 }} />}
      width={600}
      zIndex={ABOVE_ANTD_MODALS_Z_INDEX}
      footer={
        <>
          <span style={{ marginRight: 'auto', fontSize: 11.5, color: 'var(--text-subtle)' }}>
            {t('report.needsGitHub')}
          </span>
          <button type="button" className="btn" onClick={close}>
            {t('common.cancel')}
          </button>
          <button type="submit" form={FORM_ID} className="btn btn-primary" disabled={!canSubmit}>
            {opening ? t('report.opening') : t('report.openOnGitHub')}
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
            {t('report.titleLabel')}
          </label>
          <input
            id="report-title"
            className="input"
            value={title}
            maxLength={TITLE_MAX}
            placeholder={t('report.titlePlaceholder')}
            onChange={(e) => setTitle(e.target.value)}
            // oxlint-disable-next-line jsx-a11y/no-autofocus -- Dialog initial focus; ARIA APG asks that focus move into the dialog on open.
            autoFocus
          />
        </div>

        <div className="field">
          <label className="field-label field-required" htmlFor="report-description">
            {t('report.whatWentWrong')}
          </label>
          <textarea
            id="report-description"
            className="textarea report-description"
            rows={5}
            value={description}
            maxLength={DESCRIPTION_MAX}
            placeholder={t('report.descriptionPlaceholder')}
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
          aria-label={t('report.includeLog')}
        >
          <span className="option-row-text">
            <strong>{t('report.includeLog')}</strong>
            <span>{t('report.includeLogHint')}</span>
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
