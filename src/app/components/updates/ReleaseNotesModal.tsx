'use client';

import { useTranslation } from 'react-i18next';
import { Skeleton } from 'antd';
import { FileTextOutlined } from '@ant-design/icons';
import { useShallow } from 'zustand/react/shallow';
import Modal, { ABOVE_ANTD_MODALS_Z_INDEX } from '@/app/components/ui/Modal';
import UpdateNotes, { openExternal } from '@/app/components/updates/UpdateNotes';
import {
  GITHUB_RELEASES_URL,
  entriesToMarkdown,
  formatReleaseDate,
  releasesUpTo,
} from '@/app/lib/updates/releaseNotes';
import { useUpdateStore } from '@/app/stores/updateStore';

/** The release date in the UI language; English keeps the feed's own wording. */
function localReleaseDate(date: string | null, language: string): string {
  if (language === 'en') return formatReleaseDate(date);
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date ?? '');
  if (!match) return '';
  const utc = Date.UTC(Number(match[1]), Number(match[2]) - 1, Number(match[3]));
  return new Intl.DateTimeFormat(language, { dateStyle: 'long', timeZone: 'UTC' }).format(utc);
}

function ReleaseNotesDialog() {
  const s = useUpdateStore(
    useShallow((state) => ({
      feed: state.feed,
      feedStatus: state.feedStatus,
      currentVersion: state.currentVersion,
      loadFeed: state.loadFeed,
      close: state.closeReleaseNotes,
    }))
  );
  const { t, i18n } = useTranslation();
  const entries = s.feed ? releasesUpTo(s.feed, s.currentVersion) : [];

  return (
    <Modal
      zIndex={ABOVE_ANTD_MODALS_Z_INDEX}
      open
      onClose={s.close}
      title={t('settings.releaseNotes')}
      subtitle={s.currentVersion ? t('updates.youHave', { version: s.currentVersion }) : undefined}
      icon={<FileTextOutlined style={{ fontSize: 18 }} />}
      width={620}
      footer={
        <>
          <span className="left">
            <button
              type="button"
              className="btn btn-ghost"
              onClick={() => openExternal(GITHUB_RELEASES_URL)}
            >
              {t('updates.allReleasesOnGitHub')}
            </button>
          </span>
          <button type="button" className="btn btn-primary" onClick={s.close}>
            {t('common.close')}
          </button>
        </>
      }
    >
      {s.feedStatus === 'failed' && !s.feed ? (
        <div className="release-notes-failed">
          <p>{t('updates.feedFailed')}</p>
          <button type="button" className="btn" onClick={() => void s.loadFeed({ force: true })}>
            {t('common.tryAgain')}
          </button>
        </div>
      ) : entries.length === 0 ? (
        <Skeleton active paragraph={{ rows: 6 }} />
      ) : (
        <div className="release-notes-list">
          {entries.map((entry) => (
            <section key={entry.version} className="release-notes-entry">
              <header>
                <span className="release-notes-version">v{entry.version}</span>
                <span className="release-notes-date">
                  {entry.draft
                    ? t('updates.notReleased')
                    : localReleaseDate(entry.date, i18n.language)}
                </span>
              </header>
              {entry.title && <h3>{entry.title}</h3>}
              <UpdateNotes markdown={entriesToMarkdown([entry], { versionHeadings: false })} />
            </section>
          ))}
        </div>
      )}
    </Modal>
  );
}

/** Every version's notes up to the one you run, from the shared release feed. */
export default function ReleaseNotesModal() {
  const open = useUpdateStore((s) => s.releaseNotesOpen);
  return open ? <ReleaseNotesDialog /> : null;
}
