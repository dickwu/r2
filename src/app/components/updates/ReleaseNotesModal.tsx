'use client';

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
  const entries = s.feed ? releasesUpTo(s.feed, s.currentVersion) : [];

  return (
    <Modal
      zIndex={ABOVE_ANTD_MODALS_Z_INDEX}
      open
      onClose={s.close}
      title="Release notes"
      subtitle={s.currentVersion ? `You have v${s.currentVersion}.` : undefined}
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
              All releases on GitHub
            </button>
          </span>
          <button type="button" className="btn btn-primary" onClick={s.close}>
            Close
          </button>
        </>
      }
    >
      {s.feedStatus === 'failed' && !s.feed ? (
        <div className="release-notes-failed">
          <p>
            Couldn&apos;t load the release notes. Check your internet connection, then try again, or
            read them on GitHub.
          </p>
          <button type="button" className="btn" onClick={() => void s.loadFeed({ force: true })}>
            Try again
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
                  {entry.draft ? 'Not released yet' : formatReleaseDate(entry.date)}
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
