'use client';

import { useEffect, useMemo, useState } from 'react';
import { marked } from 'marked';
import { useThemeStore } from '@/app/stores/themeStore';

/** The app's own palette, since the frame cannot see the page's CSS variables. */
const PALETTES = {
  light: {
    text: '#181613',
    muted: '#6b665e',
    rule: 'rgba(20,18,16,.12)',
    code: '#efece6',
    bg: '#ffffff',
    link: '#c25f0c',
  },
  dark: {
    text: '#ece9e4',
    muted: '#a39d93',
    rule: 'rgba(255,255,255,.12)',
    code: 'rgba(255,255,255,.06)',
    bg: '#17171a',
    link: '#ff9a45',
  },
} as const;

function styleFor(theme: 'light' | 'dark'): string {
  const c = PALETTES[theme];
  // A reading measure (about 72 characters) rather than the pane's full width.
  return `
  :root { color-scheme: ${theme}; }
  body { font: 14px/1.65 Inter, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    color: ${c.text}; background: ${c.bg}; margin: 0; padding: 20px 24px; word-wrap: break-word; }
  main { max-width: 72ch; margin: 0 auto; }
  h1, h2, h3 { font-family: "Inter Tight", Inter, sans-serif; letter-spacing: -0.01em; line-height: 1.25; }
  h1 { font-size: 1.75em; } h2 { font-size: 1.35em; } h3 { font-size: 1.1em; }
  h1, h2 { border-bottom: 1px solid ${c.rule}; padding-bottom: .3em; }
  pre, code { font-family: "JetBrains Mono", ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12.5px; }
  pre { background: ${c.code}; padding: 12px 14px; border-radius: 6px; overflow: auto; }
  :not(pre) > code { background: ${c.code}; padding: .15em .35em; border-radius: 4px; }
  blockquote { margin: 0; padding: 0 1em; color: ${c.muted}; border-left: 3px solid ${c.rule}; }
  table { border-collapse: collapse; } th, td { border: 1px solid ${c.rule}; padding: 4px 10px; }
  hr { border: none; border-top: 1px solid ${c.rule}; }
  img { max-width: 100%; } a { color: ${c.link}; }
`;
}

/**
 * Rendered Markdown in a sandboxed frame: no scripts run and nothing in it
 * reaches the app, so a document's raw HTML is shown without sanitizing it.
 */
export default function MarkdownPreview({ source, height }: { source: string; height: number }) {
  const theme = useThemeStore((s) => s.theme);
  // Rendering a long document on every keystroke would stall typing.
  const [debounced, setDebounced] = useState(source);
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(source), 150);
    return () => clearTimeout(timer);
  }, [source]);

  const html = useMemo(() => {
    const body = marked.parse(debounced, { async: false, gfm: true }) as string;
    return `<!doctype html><html><head><meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src * data: blob:; style-src 'unsafe-inline'">
<base target="_blank"><style>${styleFor(theme === 'dark' ? 'dark' : 'light')}</style></head><body><main>${body}</main></body></html>`;
  }, [debounced, theme]);

  return (
    <iframe
      className="file-editor-markdown"
      title="Markdown preview"
      sandbox=""
      srcDoc={html}
      style={{ height }}
    />
  );
}
