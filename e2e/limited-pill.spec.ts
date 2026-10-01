// The header "N limited" pill and the Bulk Actions modal it opens must count the
// same workers (2026-10-01: the pill read "2 limited · reset 15:50" fifteen hours
// after that reset while the modal said "No limited workers").
import { test, expect } from './fixtures';

test('an expired rate-limit reset is not counted; a future one is, in the pill and the modal alike', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('amux_walkthrough_done', '1'));
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any).updateRateLimitPill === 'function');
  const read = () => page.evaluate(() => ({
    shown: document.getElementById('rate-limit-pill')!.classList.contains('show'),
    text: document.getElementById('rate-limit-pill-text')!.textContent,
  }));
  // Only expired resets: no pill.
  await page.evaluate(() => {
    const past = Math.floor(Date.now() / 1000) - 15 * 3600;
    // eslint-disable-next-line no-undef
    (globalThis as any).eval('sessions').splice(0, Infinity,
      { name: 'stale-a', running: false, rate_limited_until: past, rate_limit_banner: true },
      { name: 'stale-b', running: false, rate_limited_until: past, rate_limit_banner: true });
    (window as any).updateRateLimitPill();
  });
  expect((await read()).shown).toBe(false);
  // One still in the future: the pill shows exactly that one, and the modal lists it.
  await page.evaluate(() => {
    const future = Math.floor(Date.now() / 1000) + 3600;
    (globalThis as any).eval('sessions').push({ name: 'live-limit', running: true, rate_limited_until: future, rate_limit_banner: true });
    (window as any).updateRateLimitPill();
  });
  const r = await read();
  expect(r.shown).toBe(true);
  expect(r.text).toMatch(/^1 limited · reset /);
  await page.evaluate(() => (window as any).openBulkActions());
  await expect(page.getByText('No limited workers.')).toHaveCount(0);
});
