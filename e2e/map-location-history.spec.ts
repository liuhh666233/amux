// Map > Location history (AMUX-5458). The timeline is routed to a fixed fake
// day so no real location data is read or written.
import { test, expect, Page } from './fixtures';

// Open the Map sidebar and keep it open until its tabs are on screen. The map
// applies settings that arrive from the server after load, which on a phone
// can collapse the sidebar again (translateX off screen, still "visible").
async function openMapSidebar(page: Page): Promise<void> {
  await page.evaluate(() => (window as any).switchView('map'));
  await page.waitForFunction(() => (globalThis as any).eval('_mapServerLoaded') === true);
  // The saved settings arriving must not leave the setting and the screen
  // disagreeing (it said "open" over a closed phone sidebar).
  const agree = await page.evaluate(() => {
    const open = (globalThis as any).eval('_mapSettings').sidebarOpen;
    const sb = document.getElementById('map-sidebar')!;
    const shown = !sb.classList.contains('hidden') && getComputedStyle(sb).display !== 'none';
    return open === shown;
  });
  expect(agree, 'map sidebar setting disagrees with the screen after load').toBe(true);
  await expect.poll(async () => page.evaluate(() => {
    const g: any = globalThis;
    if (!g.eval('_mapSettings').sidebarOpen) g._mapToggleSidebar();
    // A display:none element measures 0x0, which would read as "on screen".
    const r = document.getElementById('map-tab-history')!.getBoundingClientRect();
    return r.width > 0 && r.height > 0 && r.left >= 0 && r.right <= window.innerWidth;
  }), {timeout: 15000, intervals: [400]}).toBe(true);
}

const DAY = {
  ok: true, measured: true, n_considered: 212, visits_considered: 1, from: 0, to: 0,
  segments: [
    {id: 'stop_home', kind: 'stop', start: 1790841600, end: 1790866800, duration_s: 25200, lat: 40.7411, lon: -73.9897, point_count: 12},
    {id: 'trip_w1', kind: 'trip', mode: 'walking', mode_confidence: 'measured', start: 1790866800, end: 1790867700,
      duration_s: 900, distance_m: 1180, from: [40.7411, -73.9897], to: [40.7505, -73.9934],
      bbox: [40.7411, -73.9934, 40.7505, -73.9897], point_count: 90,
      path: [[40.7411, -73.9897], [40.7450, -73.9910], [40.7505, -73.9934]]},
    {id: 'trip_t1', kind: 'trip', mode: 'train', mode_confidence: 'inferred', start: 1790868000, end: 1790869800,
      duration_s: 1800, distance_m: 21400, from: [40.7505, -73.9934], to: [40.9000, -73.9000],
      bbox: [40.7505, -73.9934, 40.9000, -73.9000], point_count: 110,
      path: [[40.7505, -73.9934], [40.8200, -73.9500], [40.9000, -73.9000]]},
  ],
};

test('Map > Location history lists a day, draws it, and focuses a trip', async ({ page }) => {
  let asked = '';
  await page.route('**/api/map/location/timeline**', async route => {
    asked = route.request().url();
    await route.fulfill({json: DAY});
  });
  await page.addInitScript(() => { localStorage.setItem('amux_walkthrough_done', '1'); localStorage.removeItem('amux_map_tab'); });
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any).switchView === 'function' && typeof (window as any)._mapSidebarTab === 'function');
  await openMapSidebar(page);
  const tab = page.locator('#map-tab-history');
  await expect(tab).toBeVisible();
  await expect(page.locator('#map-pins-pane')).toBeVisible();
  await tab.click();
  await expect(tab).toHaveAttribute('aria-selected', 'true');
  await expect(page.locator('#map-pins-pane')).toBeHidden();
  await expect(page.locator('#map-loc-summary')).toContainText('212 points');
  await expect(page.locator('#map-loc-summary')).toContainText('2 trips');
  const rows = page.locator('.map-loc-row');
  await expect(rows).toHaveCount(3);
  await expect(rows.nth(2)).toContainText('Train (inferred)');
  // The request asks for one local day.
  const u = new URL(asked);
  expect(Number(u.searchParams.get('to')) - Number(u.searchParams.get('from'))).toBeGreaterThanOrEqual(23 * 3600);
  // Two trips drawn as lines plus the stop and the start/end markers.
  const layers = await page.evaluate(() => { const g: any = globalThis; const l = g.eval('_locLayer'); return l ? l.getLayers().length : 0; });
  expect(layers).toBeGreaterThanOrEqual(5);
  const box = await rows.nth(1).boundingBox();
  if ((page.viewportSize()?.width || 1200) <= 600) expect(box!.height).toBeGreaterThanOrEqual(44);
  // Focusing a trip must not rewrite the saved map document (it once saved
  // sidebarOpen:false, so every later load started with the sidebar shut).
  let mapSaves = 0;
  page.on('request', req => { if (req.method() === 'POST' && req.url().includes('/api/map?replace=1')) mapSaves++; });
  await rows.nth(1).click();
  if ((page.viewportSize()?.width || 1200) <= 600) {
    // On a phone the sidebar gets out of the way so the focused trip is visible.
    await expect.poll(() => page.evaluate(() => (globalThis as any).eval('_mapSettings').sidebarOpen)).toBe(false);
    await page.waitForTimeout(400);
    expect(mapSaves, 'focusing a trip saved the map settings').toBe(0);
  } else {
    await expect(rows.nth(1)).toHaveClass(/active/);
  }
  // Previous day asks again for a different window.
  const before = asked;
  await openMapSidebar(page);
  await page.locator('.map-loc-step').first().click();
  await expect.poll(() => asked).not.toBe(before);
});

test('Map > Location history says so when a day has nothing, and shows the iPhone bridge status', async ({ page }) => {
  await page.route('**/api/map/location/timeline**', route => route.fulfill({json: {ok: true, measured: true, n_considered: 0, segments: []}}));
  await page.addInitScript(() => { localStorage.setItem('amux_walkthrough_done', '1'); localStorage.setItem('amux_map_tab', 'history'); });
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any)._mapSidebarTab === 'function');
  await openMapSidebar(page);
  await page.evaluate(() => (window as any)._mapSidebarTab('history'));
  await expect(page.locator('.map-loc-empty')).toContainText('Location history');
  // The native app calls back with its status; the switch and counts render.
  await page.evaluate(() => (window as any).__amuxNativeLocation({enabled: true, authorization: 'whenInUse', precise: false, motion: 'on', pending: 7}));
  const native = page.locator('#map-loc-native');
  await expect(native).toBeVisible();
  await expect(native).toContainText('While using the app only');
  await expect(native).toContainText('approximate');
  await expect(native).toContainText('Waiting to upload: 7');
  await expect(native.getByRole('button', {name: 'Turn off'})).toBeVisible();
});
