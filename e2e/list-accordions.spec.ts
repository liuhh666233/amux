// Ethan, 2026-10-03: "make these entire lists an accordion not expanded by
// default" (the Connection modal's interruption history and the About panel's
// token Breakdown). Both render collapsed with a count in the summary.
import { test, expect } from './fixtures';

test('connection history is collapsed with a count, and opens on tap', async ({ page }) => {
  await page.addInitScript(() => {
    localStorage.setItem('amux_walkthrough_done', '1');
    const now = Date.now();
    const ev: any[] = [];
    // Transitions as _recordConnState stores them: {ts, from, to, hid}.
    for (let i = 3; i >= 1; i--) {
      const t = now - i * 600_000;
      ev.push({ ts: t, from: 'live', to: 'offline', hid: 0 }, { ts: t + 20_000, from: 'offline', to: 'live', hid: 0 });
    }
    localStorage.setItem('amux_conn_events', JSON.stringify(ev));
  });
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any).showConnHistory === 'function');
  await page.evaluate(() => (window as any).showConnHistory());
  const list = page.locator('#conn-hist-list');
  await expect(list).toBeVisible();
  await expect(list).not.toHaveAttribute('open', '');
  await expect(list.locator('summary')).toContainText(/interruptions? · latest/);
  await list.locator('summary').click();
  await expect(list).toHaveAttribute('open', '');
});

test('token breakdown is collapsed with a worker count', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('amux_walkthrough_done', '1'));
  await page.route('**/api/stats/daily', r => r.fulfill({ json: { amux_tokens: 3000, total_tokens: 3500, sessions: [{ name: 'w1', total: 2000, amux: true }, { name: 'w2', total: 1500, amux: false }] } }));
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any).openAbout === 'function');
  await page.evaluate(() => (window as any).openAbout());
  const acc = page.locator('#daily-stats-breakdown');
  await expect(acc).toBeVisible({ timeout: 15000 });
  await expect(acc).not.toHaveAttribute('open', '');
  await expect(acc.locator('summary')).toHaveText('Breakdown · 2 workers');
});
