/**
 * The fixture's own test: does a dead `page.route` stub actually get noticed?
 *
 * e2e/fixtures.ts exists because a stub that matches zero requests is silent
 * (AF-47). `crates/amux-server/tests/e2e_route_stub_guard.rs` proves every spec
 * that stubs imports the fixture — but importing it is not the same as the
 * fixture WORKING, and a guard that only checks the import would be green over
 * a wrapper that counted nothing. This file tests the wrapper.
 *
 * All cells run against the real Playwright runner and the real fixture,
 * because the defect lives in the teardown path and nothing above it flows
 * through that path.
 */
import { test, expect, allowUnusedRoute } from './fixtures';

// CELL 1 — the whole point. `test.fail()` inverts the expectation, so this cell
// passes only if the run FAILS, and the only thing that can fail it is the
// fixture's teardown: the body does nothing that can throw.
test('a stub that matches zero requests fails the test', async ({ page }) => {
  test.fail(true, 'the fixture must reject this run at teardown (AF-47)');
  await page.route('**/api/never-requested-by-any-page', (r) =>
    r.fulfill({ contentType: 'application/json', body: '{}' }));
  await page.goto('/');
});

// CELL 2 — the opt-out must actually opt out, or the escape hatch is decorative
// and the first spec that needs it reaches for something worse.
test('a stub declared with allowUnusedRoute does not fail', async ({ page }) => {
  const pattern = '**/api/also-never-requested';
  await page.route(pattern, (r) =>
    r.fulfill({ contentType: 'application/json', body: '{}' }));
  allowUnusedRoute(page, pattern);
  await page.goto('/');
});

// CELL 3 — the control. Without it, cell 1 is equally consistent with "the
// fixture fails EVERY test that registers a route", which would be a wrapper
// that breaks all four real stubs while looking like it works.
test('a stub that DOES match does not fail', async ({ page }) => {
  // The page's own boot traffic is not a fixed contract: waiting for a boot
  // /api/sessions response timed out on some loads (CI mobile, local ios and
  // desktop repeats, 2026-09-30). Make the matching request explicitly so the
  // control measures the fixture, not the boot path.
  let hits = 0;
  await page.route('**/api/route-guard-probe', async (r) => { hits += 1;
    await r.fulfill({ contentType: 'application/json', body: '{"ok":true}' }); });
  await page.goto('/');
  const status = await page.evaluate(async () => (await fetch('/api/route-guard-probe')).status);
  expect(status).toBe(200);
  expect(hits).toBeGreaterThan(0);
});


test('a page created by context.newPage also rejects a dead stub', async ({ context }) => {
  test.fail(true, 'new tabs must have the same stub guard as the default page (AF-640)');
  const second = await context.newPage();
  await second.route('**/api/dead-new-page-probe', r => r.fulfill({body:'{}'}));
  await second.close();
});

test('a page created by context.newPage counts matched stubs', async ({ context }) => {
  const second = await context.newPage();
  await second.route('**/api/new-page-probe', r => r.fulfill({body:'new page counted'}));
  await second.goto('/');
  expect(await second.evaluate(() => fetch('/api/new-page-probe').then(r => r.text()))).toBe('new page counted');
  await second.close();
});
