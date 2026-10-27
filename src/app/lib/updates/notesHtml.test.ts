import { describe, expect, test } from 'bun:test';
import { renderNotesHtml } from './notesHtml';

describe('renderNotesHtml', () => {
  test('renders headings, lists, code, and http(s) links marked external', () => {
    const html = renderNotesHtml('## New\n\n- Edit `a.json` [docs](https://r2.lifefarmer.ca)');
    expect(html).toContain('<h2>New</h2>');
    expect(html).toContain('<code>a.json</code>');
    expect(html).toContain('<a href="https://r2.lifefarmer.ca" data-external="true">docs</a>');
  });

  test('raw HTML shows as text and unsafe links lose their href', () => {
    const html = renderNotesHtml(
      '<img src=x onerror=alert(1)> [x](javascript:alert(1)) <script>bad()</script>'
    );
    expect(html).not.toContain('<img');
    expect(html).not.toContain('<script');
    expect(html).not.toContain('javascript:');
    expect(html).toContain('&lt;script&gt;');
  });

  test('images become their alt text', () => {
    expect(renderNotesHtml('![screenshot](https://x/y.png)')).toBe('<p>screenshot</p>\n');
  });

  test('empty input renders nothing', () => {
    expect(renderNotesHtml('  \n')).toBe('');
  });
});
