// Ethan, 2026-10-03: "i need a way to try these again or evict them from my
// client". Every pending message row offers Send again and Remove; an attempted
// one asks before removal; Send again reuses the same msg_id. The worker and
// its send route are fakes: nothing reaches a real worker.
import { test, expect, Page } from './fixtures';

const W = 'zz-pending-actions';

async function boot(page: Page) {
  await page.addInitScript(() => localStorage.setItem('amux_walkthrough_done', '1'));
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any)._outboxRetryNow === 'function' && typeof (window as any)._mutateQueue === 'function');
}

async function seed(page: Page) {
  await page.evaluate(async (w) => {
    const g = window as any;
    await g._mutateQueue((rows: any[]) => {
      rows.push({ id: 'refused-1', url: '/api/sessions/' + w + '/send', options: { method: 'POST', body: JSON.stringify({ text: 'refused one', msg_id: 'm-refused' }) },
        timestamp: Date.now() - 2 * 86400000, attempted_at: Date.now() - 2 * 86400000, attempts: 1, state: 'blocked', error: '409: nothing was pasted', needs_decision: 'refused' });
      rows.push({ id: 'fresh-1', url: '/api/sessions/' + w + '/send', options: { method: 'POST', body: JSON.stringify({ text: 'fresh one', msg_id: 'm-fresh' }) },
        timestamp: Date.now(), state: 'pending', not_attempted: true });
    });
    g.eval("peekSession = '" + w + "'");
    g._peekMessagesRender();
  }, W);
}

test('a refused message shows Send again and Remove; Send again reuses its msg_id once', async ({ page }) => {
  const posts: string[] = [];
  await page.route(`**/api/sessions/${W}/send`, async r => {
    if (r.request().method() !== 'POST') return r.fulfill({ json: { ok: true } });
    posts.push(JSON.parse(r.request().postData() || '{}').msg_id);
    await r.fulfill({ json: { ok: true, submitted: true, id: 'msg-' + posts.length } });
  });
  await boot(page);
  await page.evaluate(() => { (window as any).eval('online = false'); });   // hold the replay until we press
  await seed(page);
  const row = page.locator('#peek-messages-list .pending-msg[data-pending-id="refused-1"]');
  await expect(row.locator('.pending-msg-state')).toHaveText('Not sent');
  // The Messages panel is not opened here, so assert the controls exist and
  // press them from the page; visibility belongs to the panel, not this row.
  await expect(row.locator('.pending-msg-retry')).toHaveText('Send again');
  await expect(row.locator('.pending-msg-remove')).toHaveText('Remove');
  await page.evaluate(() => { (window as any).eval('online = true'); });
  await row.locator('.pending-msg-retry').evaluate((b: HTMLElement) => b.click());
  await expect.poll(() => posts.filter(id => id === 'm-refused').length).toBe(1);
  await expect.poll(() => page.evaluate(() => (window as any).eval('offlineQueue').some((q: any) => q.id === 'refused-1'))).toBe(false);
});

test('removing an attempted message asks first; an unattempted one goes at once', async ({ page }) => {
  await boot(page);
  // Freeze replay: a sync that never settles makes runSyncBanner return early,
  // so nothing is sent while the rows are being removed.
  await page.evaluate(() => { (window as any).eval('online = false; _syncFlight = new Promise(() => {})'); });
  await seed(page);
  const refused = page.locator('#peek-messages-list .pending-msg[data-pending-id="refused-1"]');
  await refused.locator('.pending-msg-remove').evaluate((b: HTMLElement) => b.click());
  await expect(page.locator('#modal-backdrop.open #modal-msg')).toContainText('may already be with');
  await page.locator('#modal-btns').getByRole('button', { name: 'Cancel' }).click();
  expect(await page.evaluate(() => (window as any).eval('offlineQueue').some((q: any) => q.id === 'refused-1'))).toBe(true);
  await refused.locator('.pending-msg-remove').evaluate((b: HTMLElement) => b.click());
  await expect(page.locator('#modal-backdrop.open #modal-msg')).toContainText('may already be with');
  await page.locator('#modal-btns').getByRole('button', { name: 'Remove' }).click();
  await expect.poll(() => page.evaluate(() => (window as any).eval('offlineQueue').some((q: any) => q.id === 'refused-1'))).toBe(false);
  await page.locator('#peek-messages-list .pending-msg[data-pending-id="fresh-1"]').locator('.pending-msg-remove').evaluate((b: HTMLElement) => b.click());
  await expect.poll(() => page.evaluate(() => (window as any).eval('offlineQueue').some((q: any) => q.id === 'fresh-1'))).toBe(false);
});
