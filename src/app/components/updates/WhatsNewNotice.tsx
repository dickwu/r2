'use client';

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
      title: `Updated to v${to}`,
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
            <p>R2 Client is now on v{to}.</p>
          )}
          {entries.length > 0 ? (
            <button type="button" className="update-link" onClick={openAll}>
              All release notes
            </button>
          ) : (
            <button
              type="button"
              className="update-link"
              onClick={() => openExternal(githubReleaseUrl(to))}
            >
              Release notes on GitHub
            </button>
          )}
        </div>
      ),
    });
  }, [whatsNew, feed, feedStatus, notification]);

  return null;
}
