import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

/**
 * `.hit-target` is an absolutely positioned button under the visible row
 * content (z-index 0). Column-view rows already set `pointer-events: none`
 * on siblings so a click on the label reaches that button. Sidebar bucket
 * (and account) rows must do the same, or clicks on the name/icon are
 * swallowed and only the thin padding around them selects the bucket.
 */
describe('sidebar hit-target clickability', () => {
  const css = readFileSync(fileURLToPath(new URL('globals.css', import.meta.url)), 'utf8');

  function ruleBlocks(source: string): { selector: string; body: string }[] {
    const blocks: { selector: string; body: string }[] = [];
    let rest = source.replace(/\/\*[\s\S]*?\*\//g, '');
    const re = /([^{}]+)\{([^{}]*)\}/g;
    let match: RegExpExecArray | null;
    while ((match = re.exec(rest))) {
      blocks.push({ selector: match[1].trim(), body: match[2] });
    }
    return blocks;
  }

  function declaresPointerEventsNone(body: string): boolean {
    return /pointer-events\s*:\s*none/.test(body);
  }

  function selectorCoversRowSiblings(selector: string, rowClass: string): boolean {
    return selector.split(',').some((part) => {
      const p = part.trim();
      return p.includes(rowClass) && p.includes(':not(.hit-target)');
    });
  }

  test('bucket row labels do not steal clicks from the hit-target button', () => {
    const hits = ruleBlocks(css).filter(
      (block) =>
        selectorCoversRowSiblings(block.selector, '.sb-bucket-row') &&
        declaresPointerEventsNone(block.body)
    );
    expect(hits.length).toBeGreaterThan(0);
  });

  test('account row labels do not steal clicks from the hit-target button', () => {
    const hits = ruleBlocks(css).filter(
      (block) =>
        selectorCoversRowSiblings(block.selector, '.sb-account-row') &&
        declaresPointerEventsNone(block.body)
    );
    expect(hits.length).toBeGreaterThan(0);
  });

  test('bucket overflow and mount controls stay clickable', () => {
    const noneRules = ruleBlocks(css).filter(
      (block) => block.selector.includes('.sb-bucket-row') && declaresPointerEventsNone(block.body)
    );
    expect(noneRules.length).toBeGreaterThan(0);
    for (const block of noneRules) {
      for (const part of block.selector.split(',')) {
        const p = part.trim();
        if (!p.includes('.sb-bucket-row') || !declaresPointerEventsNone(block.body)) continue;
        // A rule that disables pointer events on all bucket-row children
        // must exclude the overflow menu and mount marker.
        if (p.includes(':not(.hit-target)') && !p.includes(':not(.sb-icon-btn)')) {
          expect(p).toContain(':not(.sb-icon-btn)');
        }
        if (p.includes(':not(.hit-target)') && !p.includes(':not(.sb-bucket-mounted-btn)')) {
          expect(p).toContain(':not(.sb-bucket-mounted-btn)');
        }
      }
    }
  });
});
