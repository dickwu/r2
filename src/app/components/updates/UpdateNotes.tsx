'use client';

import { useMemo, type MouseEvent } from 'react';
import { renderNotesHtml } from '@/app/lib/updates/notesHtml';
import { updaterPlugins } from '@/app/lib/updates/updaterPlugins';

export const openExternal = (url: string) => {
  void updaterPlugins()
    .openUrl(url)
    .catch(() => window.open(url, '_blank', 'noopener'));
};

/** Release notes Markdown, rendered strictly (see notesHtml.ts); links open in the browser. */
export default function UpdateNotes({
  markdown,
  compact = false,
}: {
  markdown: string;
  compact?: boolean;
}) {
  const html = useMemo(() => renderNotesHtml(markdown), [markdown]);

  const onClick = (event: MouseEvent<HTMLDivElement>) => {
    const link = (event.target as HTMLElement).closest('a');
    if (!link) return;
    event.preventDefault();
    const href = link.getAttribute('href');
    if (href && link.dataset.external === 'true') openExternal(href);
  };

  return (
    <div
      className={compact ? 'update-notes update-notes-compact' : 'update-notes'}
      onClick={onClick}
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}
