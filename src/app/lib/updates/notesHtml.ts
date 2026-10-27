import { Marked, type Tokens } from 'marked';

/**
 * Release notes are Markdown from the release feed or from an update's
 * `latest.json` body, and they render inside the app's own DOM, so the
 * renderer is strict: raw HTML shows as text, images become their alt text,
 * and only http(s) links survive. Links carry `data-external` so the dialog
 * can open them in the browser instead of navigating the app window.
 */

const escapeHtml = (value: string): string =>
  value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');

export const isSafeLink = (href: string | null | undefined): href is string =>
  typeof href === 'string' && /^https?:\/\//i.test(href.trim());

const notes = new Marked({
  async: false,
  gfm: true,
  breaks: false,
  renderer: {
    html(token: Tokens.HTML | Tokens.Tag) {
      return escapeHtml(token.text);
    },
    image(token: Tokens.Image) {
      return escapeHtml(token.text);
    },
    link(token: Tokens.Link) {
      const label = this.parser.parseInline(token.tokens);
      if (!isSafeLink(token.href)) return label;
      const title = token.title ? ` title="${escapeHtml(token.title)}"` : '';
      return `<a href="${escapeHtml(token.href.trim())}"${title} data-external="true">${label}</a>`;
    },
  },
});

export const renderNotesHtml = (markdown: string): string => {
  if (!markdown.trim()) return '';
  return notes.parse(markdown) as string;
};
