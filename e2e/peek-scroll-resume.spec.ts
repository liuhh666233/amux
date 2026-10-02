// Ethan, 2026-10-02: "i cant scroll down all the way". A worker's question
// (an AskUserQuestion picker) arrived while the terminal view was paused by a
// small upward wheel. The frame was buffered, scrolling back to the bottom
// resumed following without painting it, and every later poll saw "no change",
// so the question never appeared. No real worker is involved: the worker, its
// frames and the sessions list are routed in the page.
import { test, expect, allowUnusedRoute } from './fixtures';

const HISTORY = Array.from({ length: 160 }, (_, i) => `  earlier output line ${i}, long enough to make the pane scroll`).join('\n')
  + "\n\n⏺ I'm working out the auth path. That's the one decision that blocks the build.";
const BEFORE = "⏺ I'm working out the auth path. That's the one decision that blocks the build.\n\n⏺ AskUserQuestion";
const QUESTION = BEFORE + "\n\nWhich auth path should the app use?\n\n❯ 1. PropelAuth MCP (Recommended)\n  2. API key bridge\n  3. Type something.\n\nEnter to select · Tab/Arrow keys to navigate · Esc to cancel";

test('a frame buffered while scrolled up is painted when the reader scrolls back to the bottom', async ({ page }) => {
  let live = BEFORE;
  const worker = { name: 'zz-scroll', running: true, status: 'waiting', waiting_reason: 'user_input', lifecycle: 'active',
    provider: 'claude', model: 'claude-opus-5-5', dir: '/tmp', tags: [] };
  await page.route('**/api/events**', r => r.abort());
  allowUnusedRoute(page, '**/api/events**');
  const sessionsList = /\/api\/sessions(?:\?.*)?$/;
  await page.route(sessionsList, r => r.request().method() === 'GET' ? r.fulfill({ json: [worker] }) : r.fallback());
  allowUnusedRoute(page, sessionsList);
  await page.route('**/api/sessions/zz-scroll/**', r => r.request().method() === 'GET' ? r.fulfill({ json: {} }) : r.abort());
  allowUnusedRoute(page, '**/api/sessions/zz-scroll/**');
  await page.route('**/api/sessions/zz-scroll/peek**', r => r.fulfill({ json: r.request().url().includes('live=1')
    ? { name: 'zz-scroll', live_only: true, live, output: live }
    : { name: 'zz-scroll', history: HISTORY, live, output: live, history_lines: 162 } }));
  await page.addInitScript(() => localStorage.setItem('amux_walkthrough_done', '1'));
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any).openPeek === 'function'
    && (globalThis as any).eval('sessions').some((s: any) => s.name === 'zz-scroll'));
  await page.evaluate(() => (window as any).openPeek('zz-scroll'));
  const body = page.locator('#peek-body');
  await expect(body).toContainText('AskUserQuestion', { timeout: 15000 });

  // The reader nudges up: a real wheel gesture toward earlier output.
  // Dispatched rather than page.mouse.wheel, which mobile WebKit does not support;
  // the page's own wheel and scroll handlers see the same events either way.
  await body.evaluate(el => { el.dispatchEvent(new WheelEvent('wheel', { deltaY: -300, bubbles: true })); el.scrollTop -= 300; });
  await expect.poll(() => page.evaluate(() => (globalThis as any).eval('_peekScrollLocked'))).toBe(true);

  // The question arrives while the view is paused: it is buffered, not painted.
  live = QUESTION;
  await expect.poll(() => page.evaluate(() => (globalThis as any).eval('_peekBufferedOutput')), { timeout: 10000 }).toBe(true);
  await expect(body).not.toContainText('Enter to select');

  // Back to the bottom with the wheel: following resumes AND the question shows.
  await body.evaluate(el => { el.dispatchEvent(new WheelEvent('wheel', { deltaY: 5000, bubbles: true })); el.scrollTop = el.scrollHeight; });
  await expect(body).toContainText('Enter to select', { timeout: 15000 });
  const geo = await body.evaluate(el => {
    const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT, { acceptNode: n => n.textContent!.trim() ? 1 : 3 });
    let last: Node | null = null; while (walker.nextNode()) last = walker.currentNode;
    const r = document.createRange(); r.selectNodeContents(last!);
    const lr = r.getBoundingClientRect(), br = el.getBoundingClientRect();
    return { text: last!.textContent!.trim(), lastBottom: lr.bottom, paneBottom: br.bottom, gap: el.scrollHeight - el.scrollTop - el.clientHeight };
  });
  expect(geo.text).toContain('Enter to select');
  expect(geo.lastBottom).toBeLessThanOrEqual(geo.paneBottom + 1);
  expect(geo.gap).toBeLessThanOrEqual(2);
});
