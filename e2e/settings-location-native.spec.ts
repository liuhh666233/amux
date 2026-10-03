// Ethan, 2026-10-03: "I dont see the location history button in settings".
// Inside the iPhone app a native bridge (window.webkit.messageHandlers.amuxLocation)
// drives the recorder; Settings > Device now carries its switch. The bridge is
// faked here: no real recorder, nothing leaves the page.
import { test, expect, Page } from './fixtures';

async function boot(page: Page, withBridge: boolean) {
  await page.addInitScript((withBridge) => {
    localStorage.setItem('amux_walkthrough_done', '1');
    if (!withBridge) return;
    const st: any = { enabled: false, authorization: 'notDetermined', motion: 'unknown', mode: 'full', delivered: 0, stored: 0, pending: 0 };
    (window as any).__locOps = [];
    (window as any).webkit = { messageHandlers: { amuxLocation: { postMessage: (m: any) => {
      (window as any).__locOps.push(m.op);
      if (m.op === 'enable') { st.enabled = true; st.authorization = 'always'; }
      if (m.op === 'disable') st.enabled = false;
      setTimeout(() => (window as any).__amuxNativeLocation?.({ ...st }), 10);
    } } } };
  }, withBridge);
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any).toggleSettings === 'function');
}

async function openDevice(page: Page) {
  await page.locator('#settings-btn').click();
  await page.locator('#settings-menu .settings-tab-btn[data-stab="device"]').click();
}

test('inside the app, Settings > Device turns location history on and opens it', async ({ page }) => {
  await boot(page, true);
  await openDevice(page);
  const row = page.locator('#settings-loc-native');
  await expect(row).toBeVisible();
  await expect(row).toContainText('Recording on this iPhone: off');
  await row.locator('#settings-loc-toggle').click();
  await expect(row).toContainText('Recording on this iPhone: on');
  await expect(row.locator('#settings-loc-toggle')).toHaveText('Turn off');
  expect(await page.evaluate(() => (window as any).__locOps)).toEqual(expect.arrayContaining(['status', 'enable']));
  await row.getByRole('button', { name: 'Open location history' }).click();
  await expect(page.locator('#map-history-pane')).toBeVisible();
  await expect(page.locator('#map-tab-history')).toBeVisible();
});

test('in a plain browser the row says where recording lives and still opens the history', async ({ page }) => {
  await boot(page, false);
  await openDevice(page);
  const row = page.locator('#settings-loc-native');
  await expect(row).toBeVisible();
  await expect(row).toContainText('Recorded by the amux iPhone app');
  await expect(row.locator('#settings-loc-toggle')).toHaveCount(0);
  await row.getByRole('button', { name: 'Open location history' }).click();
  await expect(page.locator('#map-history-pane')).toBeVisible();
});

test('an old app build with no bridge is told to update, not to find a missing switch', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('amux_walkthrough_done', '1'));
  await page.setExtraHTTPHeaders({});
  await page.addInitScript(() => Object.defineProperty(navigator, 'userAgent', { get: () => 'Mozilla/5.0 (iPhone) Mobile/15E148 AmuxApp' }));
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any)._locEmptyHint === 'function');
  expect(await page.evaluate(() => (window as any)._locEmptyHint())).toContain('Update it in <b>TestFlight</b>');
});
