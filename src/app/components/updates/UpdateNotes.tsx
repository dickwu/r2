'use client';

import { useEffect, useMemo, useRef } from 'react';
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
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    const onClick = (event: Event) => {
      const target = event.target as HTMLElement | null;
      const link = target?.closest('a');
      if (!link) return;
      event.preventDefault();
      const href = link.getAttribute('href');
      if (href && link.dataset.external === 'true') openExternal(href);
    };
    root.addEventListener('click', onClick);
    return () => root.removeEventListener('click', onClick);
  }, [html]);

  return (
    <div
      ref={rootRef}
      className={compact ? 'update-notes update-notes-compact' : 'update-notes'}
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}
