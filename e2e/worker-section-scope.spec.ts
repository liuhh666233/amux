// A group pill narrows the Paused/Archived sections; the header must say so and
// give the fleet total (Ethan, 2026-10-01: "i see only 3 paused workers and 9
// archived, there should be many many more").
import { test, expect } from './fixtures';

test('a group filter narrows the worker sections and the headers say so, with the total', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('amux_walkthrough_done', '1'));
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any)._renderPausedSection === 'function');
  const footers = () => page.evaluate(() => ({
    paused: document.querySelector('#paused-section .paused-footer')?.textContent?.replace(/\s+/g, ' ').trim(),
    archived: document.querySelector('#archived-section .archived-footer')?.textContent?.replace(/\s+/g, ' ').trim(),
  }));
  await page.evaluate(() => {
    const g = (globalThis as any);
    const list = g.eval('sessions');
    const mk = (name: string, tags: string[], extra: any) => ({ name, tags, dir: '/tmp', running: false, ...extra });
    list.splice(0, Infinity,
      mk('p-a1', ['alpha'], { lifecycle: 'paused' }), mk('p-a2', ['alpha'], { lifecycle: 'paused' }),
      mk('p-b1', ['beta'], { lifecycle: 'paused' }), mk('p-b2', ['beta'], { lifecycle: 'paused' }), mk('p-b3', ['beta'], { lifecycle: 'paused' }),
      mk('x-a1', ['alpha'], { lifecycle: 'archived', archived: true }),
      mk('x-b1', ['beta'], { lifecycle: 'archived', archived: true }), mk('x-b2', ['beta'], { lifecycle: 'archived', archived: true }));
    g.eval('activeTag = ""; hiddenTags.clear(); filterProviders.clear(); filterModels.clear(); searchQuery = ""');
    g._renderPausedSection(); g._renderArchivedSection();
  });
  let f = await footers();
  expect(f.paused).toContain('5 paused');
  expect(f.paused).not.toContain('total');
  await page.evaluate(() => {
    const g = (globalThis as any);
    g.eval('activeTag = "alpha"');
    g._renderPausedSection(); g._renderArchivedSection();
  });
  f = await footers();
  expect(f.paused).toContain('2 paused');
  expect(f.paused).toContain('(group alpha · 5 total)');
  expect(f.archived).toContain('(group alpha · 3 total)');
});
