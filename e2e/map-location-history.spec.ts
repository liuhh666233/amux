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

const STATS = {
  ok: true, measured: true, n_considered: 5400, days_with_data: 3, bucket: 'day',
  totals: {
    walking: {distance_m: 6200, moving_s: 4400, trips: 5, avg_speed_mps: 1.41},
    driving: {distance_m: 48000, moving_s: 3600, trips: 2, avg_speed_mps: 13.3},
  },
  buckets: [{bucket: '2026-09-29', modes: {walking: {distance_m: 3000, moving_s: 2000, trips: 2, avg_speed_mps: 1.5}}},
    {bucket: '2026-09-30', modes: {driving: {distance_m: 48000, moving_s: 3600, trips: 2, avg_speed_mps: 13.3}}}],
  top_places: [{id: 'place_1_2', lat: 40.7411, lon: -73.9897, time_s: 86000, visits: 3, new: false}],
  new_places: [{id: 'place_3_4', lat: 40.9, lon: -73.9, time_s: 3600, visits: 1, new: true}],
  places_considered: 2,
  longest_trip: {id: 'trip_d1', kind: 'trip', mode: 'driving', start: 1790866800, end: 1790868600, duration_s: 1800, distance_m: 30000},
};

test('Map > Location history shows stats, a heatmap layer and raw export links', async ({ page }) => {
  let statsAsked = '';
  await page.route('**/api/map/location/timeline**', route => route.fulfill({json: {ok: true, measured: true, n_considered: 0, segments: []}}));
  await page.route('**/api/map/location/stats**', async route => { statsAsked = route.request().url(); await route.fulfill({json: STATS}); });
  await page.route('**/api/map/location/heatmap**', route => route.fulfill({json: {ok: true, measured: true, n_considered: 900, cell_deg: 0.00045,
    n_cells: 3, truncated: false, cells: [[40.7411, -73.9897, 600], [40.75, -73.99, 250], [40.9, -73.9, 50]]}}));
  await page.route('**/api/map/location/export**', route => route.fulfill({body: 'id,ts\n', contentType: 'text/csv'}));
  await page.addInitScript(() => { localStorage.setItem('amux_walkthrough_done', '1'); localStorage.setItem('amux_map_tab', 'history'); });
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any)._mapSidebarTab === 'function');
  await openMapSidebar(page);
  await page.evaluate(() => (window as any)._mapSidebarTab('history'));
  // Stats: per-mode distance and speed, places, longest trip.
  await page.locator('#map-loc-view-stats').click();
  await expect(page.locator('#map-loc-view-stats')).toHaveAttribute('aria-pressed', 'true');
  await expect(page.locator('#map-loc-daypane')).toBeHidden();
  const body = page.locator('#map-loc-stats-body');
  await expect(body).toContainText('5400 points');
  await expect(body).toContainText('Driving');
  await expect(body).toContainText('47.9 km/h');
  await expect(body).toContainText('Longest trip');
  await expect(body).toContainText('New places: 1');
  const q = new URL(statsAsked).searchParams;
  expect(Number(q.get('to')) - Number(q.get('from'))).toBe(30 * 86400);
  expect(q.get('tz_offset_min')).not.toBeNull();
  // Heatmap: one canvas circle per cell on the map.
  await openMapSidebar(page);
  await page.locator('#map-loc-heat').click();
  await expect(page.locator('#map-loc-heat')).toHaveAttribute('aria-pressed', 'true');
  await expect.poll(() => page.evaluate(() => { const l = (globalThis as any).eval('_locHeatLayer'); return l ? l.getLayers().length : 0; })).toBe(3);
  // Export asks for the shown window in the chosen format.
  await page.locator('#map-loc-view-day').click();
  const exported = page.waitForRequest(r => r.url().includes('/api/map/location/export'));
  await page.locator('.map-loc-export').getByRole('button', {name: 'CSV'}).click();
  const eu = new URL((await exported).url());
  expect(eu.searchParams.get('format')).toBe('csv');
  expect(Number(eu.searchParams.get('to')) - Number(eu.searchParams.get('from'))).toBeGreaterThanOrEqual(23 * 3600);
});

test('Map > Location history shows the iPhone recording detail and delivered vs stored', async ({ page }) => {
  await page.route('**/api/map/location/timeline**', route => route.fulfill({json: {ok: true, measured: true, n_considered: 0, segments: []}}));
  await page.addInitScript(() => { localStorage.setItem('amux_walkthrough_done', '1'); localStorage.setItem('amux_map_tab', 'history'); });
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any)._mapSidebarTab === 'function');
  await openMapSidebar(page);
  await page.evaluate(() => (window as any)._mapSidebarTab('history'));
  await page.evaluate(() => (window as any).__amuxNativeLocation({enabled: true, authorization: 'always', precise: true, motion: 'on',
    pending: 0, mode: 'full', delivered: 1200, stored: 1198}));
  const native = page.locator('#map-loc-native');
  await expect(native).toContainText('Fixes delivered / stored: 1200 / 1198');
  await expect(native).toContainText('2 not stored');
  await expect(native.getByRole('button', {name: 'Full detail'})).toHaveAttribute('aria-pressed', 'true');
  await page.evaluate(() => (window as any).__amuxNativeLocation({enabled: true, authorization: 'always', mode: 'saver', delivered: 5, stored: 5}));
  await expect(native.getByRole('button', {name: 'Battery saver'})).toHaveAttribute('aria-pressed', 'true');
  await expect(native).not.toContainText('not stored');
});
