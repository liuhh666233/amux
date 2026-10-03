// Ethan, 2026-10-03: "it's stuck here" at "Asking this device for permission…".
// getCurrentPosition's own timeout does not run while the permission request is
// unanswered; the dashboard bounds the whole request. Here the request never
// answers (stubbed), and the clock is fast-forwarded past the bound.
import { test, expect } from './fixtures';

test('an unanswered location permission ends with advice instead of hanging', async ({ page }) => {
  await page.addInitScript(() => {
    localStorage.setItem('amux_walkthrough_done', '1');
    Object.defineProperty(navigator, 'geolocation', { configurable: true, value: { getCurrentPosition: () => {}, watchPosition: () => 0, clearWatch: () => {} } });
  });
  await page.clock.install();
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any)._settingsToggleGeo === 'function');
  await page.locator('#settings-btn').click();
  await page.locator('#settings-menu .settings-tab-btn[data-stab="device"]').click();
  await page.locator('#settings-geo-btn').click();
  await expect(page.locator('#settings-geo-status')).toHaveText('Asking this device for permission…');
  await page.clock.fastForward(21_000);
  await expect(page.locator('#settings-geo-status')).toContainText('Not enabled: this browser did not answer the permission request');
  await expect(page.locator('#settings-geo-btn')).toHaveText('Attach location to my messages');
});
