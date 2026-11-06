'use client';

import { useTranslation } from 'react-i18next';
import { useEffect } from 'react';
import { App } from 'antd';
import { useShallow } from 'zustand/react/shallow';
import UpdateNotes, { openExternal } from '@/app/components/updates/UpdateNotes';
import {
  entriesToMarkdown,
  githubReleaseUrl,
  releasesBetween,
} from '@/app/lib/updates/releaseNotes';
import { useUpdateStore } from '@/app/stores/updateStore';

const KEY = 'whats-new';

/**
 * On the first launch after an update, one note in the corner with what the
 * new version brings. It waits for the release feed, and falls back to a
 * GitHub link when the feed can't be read.
 */
export default function WhatsNewNotice() {
  const { notification } = App.useApp();
  const { t } = useTranslation();
  const { whatsNew, feed, feedStatus } = useUpdateStore(
    useShallow((s) => ({ whatsNew: s.whatsNew, feed: s.feed, feedStatus: s.feedStatus }))
  );

  useEffect(() => {
    if (!whatsNew || feedStatus === 'loading' || feedStatus === 'idle') return;
    const { from, to } = whatsNew;
    const entries = feed ? releasesBetween(feed, from, to) : [];
    const store = useUpdateStore.getState();
    store.dismissWhatsNew();

    const openAll = () => {
      notification.destroy(KEY);
      useUpdateStore.getState().openReleaseNotes();
    };

    notification.open({
      key: KEY,
      title: t('updates.whatsNew.title', { version: to }),
      duration: 0,
      placement: 'bottomRight',
      description: (
        <div className="whats-new">
          {entries.length > 0 ? (
            <UpdateNotes
              compact
              markdown={entriesToMarkdown(entries, { versionHeadings: entries.length > 1 })}
            />
          ) : (
            <p>{t('updates.whatsNew.fallback', { version: to })}</p>
          )}
          {entries.length > 0 ? (
            <button type="button" className="update-link" onClick={openAll}>
              {t('updates.allReleaseNotes')}
            </button>
          ) : (
            <button
              type="button"
              className="update-link"
              onClick={() => openExternal(githubReleaseUrl(to))}
            >
              {t('updates.releaseNotesOnGitHub')}
            </button>
          )}
        </div>
      ),
    });
  }, [whatsNew, feed, feedStatus, notification, t]);

  return null;
}
