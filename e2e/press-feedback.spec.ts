// AMUX-5417 press contract (Ethan, 2026-10-01: "across the entire amux ui/ux
// audit every button press and make sure theres progress indicators/feedback").
//
// The shared layer in state/feedback.mjs owns this for every control, so these
// tests drive REAL controls on several surfaces with routed, delayed or failing
// API responses and assert what the user sees:
//   busy within the press (disabled + aria-busy + .press-busy),
//   no double-fire, and an outcome (outline and/or toast) on success AND failure.
// No real data changes: every write the tests make is routed and fulfilled here.
import { test, expect, Page } from './fixtures';

async function boot(page: Page): Promise<void> {
  await page.addInitScript(() => localStorage.setItem('amux_walkthrough_done', '1'));
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any).openSchedModal === 'function'
    && typeof (window as any).__amuxState?.interactions?.recent === 'function');
}

async function fillShellSchedule(page: Page, title: string): Promise<void> {
  await page.evaluate(() => (window as any).openSchedModal());
  await expect(page.locator('#sched-save-btn')).toBeVisible();
  await page.fill('#sched-title', title);
  await page.check('#sched-kind-shell');
  await page.fill('#sched-command', 'true');
}

// Writes go to the route; reads fall through to the real server.
async function routeWrites(page: Page, pattern: string, handler: (route: any) => Promise<void>): Promise<{posts: number}> {
  const seen = {posts: 0};
  await page.route(pattern, async route => {
    if (route.request().method() === 'GET') return route.fallback();
    seen.posts++;
    await handler(route);
  });
  return seen;
}

test('schedule Save: busy while the request runs, no double-fire, then an outcome', async ({page}, testInfo) => {
  await boot(page);
  const seen = await routeWrites(page, '**/api/schedules', async route => {
    await new Promise(r => setTimeout(r, 1500));
    await route.fulfill({json: {ok: true, id: 'SCHED-press-test', title: 'press test'}});
  });
  await fillShellSchedule(page, 'press test');
  const save = page.locator('#sched-save-btn');
  await save.click();
  // Received within the press: the layer marks it before the response exists.
  await expect(save).toHaveAttribute('aria-busy', 'true', {timeout: 300});
  await expect(save).toBeDisabled();
  await expect(save).toHaveClass(/press-busy/);
  await page.screenshot({path: testInfo.outputPath('schedule-save-busy.png')});
  // A second press while busy must not send a second request. The native
  // disabled state blocks a real click; a scripted click on the element is the
  // harshest version (it bypasses pointer hit-testing), and must not fire either.
  await page.evaluate(() => (document.getElementById('sched-save-btn') as HTMLButtonElement).click());
  await expect.poll(() => seen.posts, {timeout: 5000}).toBe(1);
  // Outcome: the dialog closes on success, so the check is not visible there;
  // the layer says it instead.
  await expect(page.locator('#toast')).toContainText(/done/i, {timeout: 5000});
  await expect(save).not.toHaveAttribute('aria-busy', 'true');
  expect(seen.posts).toBe(1);
  await page.screenshot({path: testInfo.outputPath('schedule-save-done.png')});
});

test('schedule Save failure: red outline and the server sentence, never silence', async ({page}, testInfo) => {
  await boot(page);
  await routeWrites(page, '**/api/schedules', async route => {
    await new Promise(r => setTimeout(r, 400));
    // 4xx is a REFUSAL. (A 5xx is kept and retried by the outbox, by design;
    // that press settles as queued, covered by the next test.)
    await route.fulfill({status: 409, json: {error: 'Schedule store is read-only'}});
  });
  await fillShellSchedule(page, 'press failure');
  const save = page.locator('#sched-save-btn');
  await save.click();
  await expect(save).toHaveAttribute('aria-busy', 'true', {timeout: 300});
  await expect(save).toHaveClass(/press-failed/, {timeout: 5000});
  await expect(page.locator('#toast')).toContainText('Schedule store is read-only');
  // The dialog stays open with the button usable again, so the user can retry.
  await expect(save).toBeVisible();
  await expect(save).toBeEnabled();
  await page.screenshot({path: testInfo.outputPath('schedule-save-failed.png')});
});

test('a server error keeps the change for retry and the press says so (queued, not silent)', async ({page}) => {
  await boot(page);
  // Delayed like every other press test. An instant 503 settled before the
  // first busy poll on CI's faster runners (aria-busy already "false" while
  // data-command-observed="true" showed the layer had bound the press).
  await routeWrites(page, '**/api/schedules', async route => {
    await new Promise(r => setTimeout(r, 1200));
    await route.fulfill({status: 503, json: {error: 'restarting'}});
  });
  await fillShellSchedule(page, 'press queued');
  const save = page.locator('#sched-save-btn');
  await save.click();
  await expect(save).toHaveAttribute('aria-busy', 'true', {timeout: 300});
  await expect(save).toHaveClass(/press-queued/, {timeout: 5000});
  await expect(save).toBeEnabled();
});

test('board Add: the dialog closes at once, the outcome still shows, and a refusal removes the card', async ({page}, testInfo) => {
  await boot(page);
  let refuse = false;
  await routeWrites(page, '**/api/board', async route => {
    await new Promise(r => setTimeout(r, 600));
    if (refuse) return route.fulfill({status: 409, json: {error: 'Board is read-only for this test'}});
    return route.fulfill({json: {id: 'PRESS-1', title: 'press card', status: 'backlog', tags: [], created: 1, updated: 1}});
  });
  const add = async (title: string) => {
    await page.evaluate(() => (window as any).openBoardAdd('backlog'));
    await page.fill('#be-title', title);
    await page.locator('#board-edit-overlay .be-save').click();
  };
  await add('press card');
  await expect(page.locator('#toast')).toContainText(/done/i, {timeout: 5000});
  refuse = true;
  await add('refused card');
  await expect(page.locator('#toast')).toContainText('Board is read-only for this test', {timeout: 5000});
  // boardItems is a script-level `let`, not a window property: read it by name.
  await expect.poll(() => page.evaluate(() => (0, eval)('boardItems').some((i: any) => i.title === 'refused card')),
    {timeout: 5000}).toBe(false);
  await page.screenshot({path: testInfo.outputPath('board-add-refused.png')});
});

test('a handler that awaits before its request still gets the busy state', async ({page}) => {
  await boot(page);
  // A THIRD of the command handlers await something first (a draft save, a
  // lookup). Exercise exactly that shape through the real layer: a button whose
  // handler waits 300ms and then writes. The registry kind marks it a command
  // control, the same way declare() marks real ones.
  // A dedicated path: the app's own prefs bootstrap posts to /api/prefs, and a
  // stray write there would be a second request in this window.
  const seen = await routeWrites(page, '**/api/press-test-late', async route => {
    await new Promise(r => setTimeout(r, 1200));
    await route.fulfill({json: {ok: true}});
  });
  await page.evaluate(() => {
    const b = document.createElement('button');
    b.id = 'press-late'; b.className = 'btn'; b.textContent = 'Late save';
    b.dataset.interactionKind = 'command.pressLate';
    b.onclick = async () => {
      await new Promise(r => setTimeout(r, 300));
      await fetch('/api/press-test-late', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({ok: 1})});
    };
    document.body.appendChild(b);
  });
  const late = page.locator('#press-late');
  await late.click();
  await expect(late).toHaveAttribute('aria-busy', 'true', {timeout: 1000});
  await expect(late).toBeDisabled();
  await expect(late).toHaveClass(/press-done/, {timeout: 5000});
  expect(seen.posts).toBe(1);
});

test('a handler that fails SILENTLY still reaches a toast with the server sentence', async ({page}) => {
  await boot(page);
  // Plain fetch, no apiCall, no toast of its own: the layer is the only thing
  // that can tell the user. (apiCall surfaces already toast; this pins the
  // layer's own failure toast, which those surfaces mask.)
  await routeWrites(page, '**/api/press-test-silent', async route => {
    await new Promise(r => setTimeout(r, 300));
    await route.fulfill({status: 409, json: {error: 'Refused for the silent test'}});
  });
  await page.evaluate(() => {
    const b = document.createElement('button');
    b.id = 'press-silent'; b.className = 'btn'; b.textContent = 'Silent save';
    b.dataset.interactionKind = 'command.pressSilent';
    b.onclick = () => { fetch('/api/press-test-silent', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: '{}'}); };
    document.body.appendChild(b);
  });
  const silent = page.locator('#press-silent');
  await silent.click();
  await expect(silent).toHaveClass(/press-failed/, {timeout: 5000});
  await expect(page.locator('#toast')).toContainText('Silent save: Refused for the silent test', {timeout: 5000});
});

test('navigation that never makes a request is not slowed down: quick repeat presses all run', async ({page}) => {
  await boot(page);
  await page.evaluate(() => {
    (window as any).__pressNav = 0;
    const b = document.createElement('button');
    b.id = 'press-nav'; b.textContent = 'Next';
    b.dataset.interactionKind = 'command.pressNav';   // over-approximated as a command, like peekMsgNext
    b.onclick = () => { (window as any).__pressNav++; };
    document.body.appendChild(b);
  });
  for (let i = 0; i < 4; i++) await page.locator('#press-nav').click();
  expect(await page.evaluate(() => (window as any).__pressNav)).toBe(4);
});
