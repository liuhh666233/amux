// A group filter narrows the Paused/Archived sections; the header must say so and
// give the fleet total (Ethan, 2026-10-01: "i see only 3 paused workers and 9
// archived, there should be many many more").
//
// The fleet is served by a routed /api/sessions (the live event stream is cut),
// so no background refresh can replace it mid-test (ios-safari, run 36962337886).
import { test, expect } from './fixtures';

const mk = (name: string, tags: string[], extra: any) => ({ name, tags, dir: '/tmp', running: false, ...extra });
const FLEET = [
  mk('p-a1', ['alpha'], { lifecycle: 'paused' }), mk('p-a2', ['alpha'], { lifecycle: 'paused' }),
  mk('p-b1', ['beta'], { lifecycle: 'paused' }), mk('p-b2', ['beta'], { lifecycle: 'paused' }), mk('p-b3', ['beta'], { lifecycle: 'paused' }),
  mk('x-a1', ['alpha'], { lifecycle: 'archived', archived: true }),
  mk('x-b1', ['beta'], { lifecycle: 'archived', archived: true }), mk('x-b2', ['beta'], { lifecycle: 'archived', archived: true }),
];

test('a group filter narrows the worker sections and the headers say so, with the total', async ({ page }) => {
  await page.route('**/api/events**', route => route.abort());
  await page.route(/\/api\/sessions(\?.*)?$/, route => route.request().method() === 'GET'
    ? route.fulfill({ json: FLEET }) : route.fallback());
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any)._renderPausedSection === 'function'
    && typeof (window as any).fetchSessions === 'function');
  const footers = () => page.evaluate(() => ({
    paused: document.querySelector('#paused-section .paused-footer')?.textContent?.replace(/\s+/g, ' ').trim() || '',
    archived: document.querySelector('#archived-section .archived-footer')?.textContent?.replace(/\s+/g, ' ').trim() || '',
  }));
  const show = (tag: string) => page.evaluate(async t => {
    const g = globalThis as any;
    // Filter first, so whichever render runs last (this one or the fetch's) uses it.
    g.eval(`activeTag = ${JSON.stringify(t)}; hiddenTags.clear(); filterProviders.clear(); filterModels.clear(); searchQuery = ""`);
    await g.fetchSessions();
    g._renderPausedSection(); g._renderArchivedSection();
  }, tag);
  await show('');
  await expect.poll(async () => (await footers()).paused).toContain('5 paused');
  expect((await footers()).paused).not.toContain('total');
  await show('alpha');
  await expect.poll(async () => (await footers()).paused).toContain('2 paused');
  const f = await footers();
  expect(f.paused).toContain('(group alpha · 5 total)');
  expect(f.archived).toContain('(group alpha · 3 total)');
});
