// Ethan, 2026-10-03: "be more conservative with real estate, accordion in the
// settings at the bottom; also remove the settings from device settings, it
// should be here". The recorder controls and export sit in a collapsed
// Settings section at the bottom of Map > Location history; its summary keeps
// the recording state visible. A fake native bridge stands in for the app.
import { test, expect, Page } from './fixtures';

async function boot(page: Page) {
  await page.addInitScript(() => {
    localStorage.setItem('amux_walkthrough_done', '1');
    const st: any = { enabled: false, authorization: 'always', motion: 'on', mode: 'full', delivered: 0, stored: 0, pending: 51 };
    (window as any).webkit = { messageHandlers: { amuxLocation: { postMessage: (m: any) => {
      if (m.op === 'enable') st.enabled = true;
      if (m.op === 'disable') st.enabled = false;
      setTimeout(() => (window as any).__amuxNativeLocation?.({ ...st }), 10);
    } } } };
  });
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any)._mapSidebarTab === 'function');
}

test('recorder controls live in a collapsed Settings section at the bottom of Location history', async ({ page }) => {
  await boot(page);
  await page.evaluate(() => { (window as any).switchView('map'); });
  await page.evaluate(() => { const sb = document.getElementById('map-sidebar'); if (sb && sb.classList.contains('hidden')) (window as any)._mapToggleSidebar(); (window as any)._mapSidebarTab('history'); });
  const acc = page.locator('#map-loc-settings');
  await expect(acc).toBeVisible();
  await expect(acc).not.toHaveAttribute('open', '');
  await expect(page.locator('#map-loc-native')).toBeHidden();
  await expect(page.locator('#map-loc-settings-sum')).toHaveText('· This iPhone: recording off · 51 waiting');
  // Below the day view, not above it.
  const accTop = await acc.evaluate(e => e.getBoundingClientRect().top);
  const viewsTop = await page.locator('#map-loc-view-day').evaluate(e => e.getBoundingClientRect().top);
  expect(accTop).toBeGreaterThan(viewsTop);
  await acc.locator('summary').click();
  await page.locator('#map-loc-native').getByRole('button', { name: 'Turn on' }).click();
  await expect(page.locator('#map-loc-settings-sum')).toHaveText('· This iPhone: recording on · 51 waiting');
  await expect(acc.getByRole('button', { name: 'GeoJSON' })).toBeVisible();
});

test('Settings > Device no longer carries a location row', async ({ page }) => {
  await boot(page);
  await page.locator('#settings-btn').click();
  await page.locator('#settings-menu .settings-tab-btn[data-stab="device"]').click();
  await expect(page.locator('#settings-loc-native')).toHaveCount(0);
});
