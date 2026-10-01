// Ethan, 2026-10-01 (iPhone): the peek header's NEEDS INPUT chip was squeezed
// between the worker name and the model pill and clipped mid-word; tapping it
// must open the card with somewhere to answer. And the group row's Reset goes
// first. No real data changes: the worker, card and every write are fakes.
import { test, expect, Page } from './fixtures';

const ASK = 'Should I go ahead with the shard move to the larger spot instance tonight, or hold until tomorrow?';

async function boot(page: Page) {
  await page.addInitScript(() => localStorage.setItem('amux_walkthrough_done', '1'));
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any).openPeek === 'function' && typeof (window as any)._bdOpenAnswer === 'function');
}

test('needs-input chip has its own row, is not clipped, and opens the card with the answer focused', async ({ page }) => {
  await boot(page);
  const card: any = { id: 'NI-9001', title: 'Shard move decision', status: 'needsyou', session: 'ni-worker', desc: '', archived: false };
  const sent: any[] = [];
  const patches: any[] = [];
  await page.route('**/api/sessions/ni-worker/send', async r => { sent.push(JSON.parse(r.request().postData() || '{}')); await r.fulfill({ json: { ok: true, submitted: true } }); });
  await page.route('**/api/sessions/ni-worker/**', r => r.request().method() === 'GET' ? r.fulfill({ json: { ok: true, output: '', history: '' } }) : r.fallback());
  await page.route('**/api/board/NI-9001**', async r => {
    if (r.request().method() === 'PATCH') {
      const p = JSON.parse(r.request().postData() || '{}');
      patches.push(p);
      if (p.desc_append) card.desc += '\n' + p.desc_append;
      if (p.status) card.status = p.status;
      return r.fulfill({ json: { ok: true, ...card } });
    }
    return r.fulfill({ json: card });
  });
  await page.route('**/api/needs-input/log', r => r.fulfill({ json: { ok: true } }));
  await page.evaluate(({ card, ask }) => {
    const g = globalThis as any;
    g.eval('sessions').push({ name: 'ni-worker', running: true, status: 'waiting', waiting_reason: 'owner', lifecycle: 'active',
      owner_block: { card: card.id, ask }, tags: [], dir: '/tmp', flags: '', provider: 'claude', model: 'claude-opus-5-5' });
    g.eval('boardItems').push({ ...card });
    g.openPeek('ni-worker');
  }, { card, ask: ASK });
  await page.waitForFunction(() => (document.getElementById('peek-overlay') as HTMLElement).dataset.session === 'ni-worker');
  await page.evaluate(() => (globalThis as any).updatePeekStatus());
  const row = page.locator('#peek-needs-input-row');
  await expect(row).toBeVisible();
  const chip = row.locator('.status-badge.needs-input');
  await expect(chip).toBeVisible();
  // Its own row, below the title row; inside the viewport; not clipped.
  const geo = await page.evaluate(() => {
    const r = document.getElementById('peek-needs-input-row')!.getBoundingClientRect();
    const t = document.getElementById('peek-title-row')!.getBoundingClientRect();
    const c = document.querySelector('#peek-needs-input-row .status-badge.needs-input') as HTMLElement;
    const cr = c.getBoundingClientRect();
    return { rowTop: r.top, titleBottom: t.bottom, left: cr.left, right: cr.right, vw: innerWidth, h: cr.height,
      titleHasChip: !!document.querySelector('#peek-title-row .status-badge.needs-input') };
  });
  expect(geo.rowTop).toBeGreaterThanOrEqual(geo.titleBottom - 1);
  expect(geo.left).toBeGreaterThanOrEqual(0);
  expect(geo.right).toBeLessThanOrEqual(geo.vw + 0.5);
  expect(geo.h).toBeGreaterThanOrEqual(44);
  expect(geo.titleHasChip).toBe(false);
  await expect(page.locator('#peek-title-row')).toContainText('needs input');
  // Tap: the card opens with the answer box focused.
  await chip.click();
  await expect(page.locator('#board-detail-overlay')).toHaveClass(/active/);
  await expect(page.locator('#bd-answer')).toBeVisible();
  await expect(page.locator('#bd-answer-ask')).toContainText('shard move');
  await expect(page.locator('#bd-answer-text')).toBeFocused();
  // Answer: through the triage path, to the worker and onto the card.
  await page.fill('#bd-answer-text', 'Go ahead tonight.');
  await page.click('#bd-answer-send');
  await expect(page.locator('#toast')).toContainText('Answered', { timeout: 10000 });
  expect(sent).toHaveLength(1);
  expect(sent[0].text).toContain('Go ahead tonight.');
  expect(String(sent[0].msg_id)).toMatch(/^triage-/);
  expect(patches.some(p => String(p.desc_append || '').includes('Owner reply by owner in the card: Go ahead tonight.'))).toBe(true);
  expect(patches.some(p => p.status === 'todo')).toBe(true);
  await expect(page.locator('#bd-answer')).toBeHidden();
});

test('group row: Reset is the first control when a group is active, absent otherwise', async ({ page }) => {
  await boot(page);
  const first = () => page.evaluate(() => {
    const el = document.getElementById('tag-filters')!.firstElementChild as HTMLElement | null;
    return { cls: el?.className || '', text: el?.textContent || '', resets: document.querySelectorAll('#tag-filters .tag-reset-btn').length };
  });
  await page.evaluate(() => {
    const g = globalThis as any;
    const list = g.eval('sessions');
    list.push({ name: 'gr-a', tags: ['alpha'], running: true, status: 'idle', lifecycle: 'active', dir: '/tmp' },
               { name: 'gr-b', tags: ['beta'], running: true, status: 'idle', lifecycle: 'active', dir: '/tmp' });
    g.eval('activeTag = ""; hiddenTags.clear()');
    g.render();
  });
  expect((await first()).resets).toBe(0);
  await page.evaluate(() => { (globalThis as any).eval('activeTag = "alpha"'); (globalThis as any).render(); });
  const f = await first();
  expect(f.cls).toContain('tag-reset-btn');
  expect(f.text).toContain('Reset');
  expect(f.resets).toBe(1);
});
