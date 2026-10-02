// The header "N limited" pill and the Bulk Actions modal it opens must count the
// same workers (2026-10-01: the pill read "2 limited · reset 15:50" fifteen hours
// after that reset while the modal said "No limited workers").
//
// The fleet is served by a routed /api/sessions (and the live event stream is
// cut), so a background refresh cannot replace the fixture mid-test: injecting
// into `sessions` lost that race on ios-safari (run 36962337886).
import { test, expect } from './fixtures';

const now = () => Math.floor(Date.now() / 1000);

test('an expired rate-limit reset is not counted; a future one is, in the pill and the modal alike', async ({ page }) => {
  let fleet: any[] = [
    { name: 'stale-a', running: false, lifecycle: 'active', rate_limited_until: now() - 15 * 3600, rate_limit_banner: true },
    { name: 'stale-b', running: false, lifecycle: 'active', rate_limited_until: now() - 15 * 3600, rate_limit_banner: true },
  ];
  await page.route('**/api/events**', route => route.abort());
  await page.route(/\/api\/sessions(\?.*)?$/, route => route.request().method() === 'GET'
    ? route.fulfill({ json: fleet }) : route.fallback());
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any).updateRateLimitPill === 'function'
    && typeof (window as any).fetchSessions === 'function');
  const refresh = () => page.evaluate(async () => { await (window as any).fetchSessions(); (window as any).updateRateLimitPill(); });
  const read = () => page.evaluate(() => ({
    shown: document.getElementById('rate-limit-pill')!.classList.contains('show'),
    text: document.getElementById('rate-limit-pill-text')!.textContent,
  }));
  await refresh();
  await expect.poll(async () => (await read()).shown).toBe(false);
  fleet = [...fleet, { name: 'live-limit', running: true, lifecycle: 'active', rate_limited_until: now() + 3600, rate_limit_banner: true }];
  await refresh();
  await expect.poll(async () => (await read()).shown).toBe(true);
  expect((await read()).text).toMatch(/^1 limited · reset /);
  await page.evaluate(() => (window as any).openBulkActions());
  await expect(page.getByText('No limited workers.')).toHaveCount(0);
});
