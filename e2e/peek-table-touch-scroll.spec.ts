// Ethan, 2026-10-03 23:48: "on mobile its hard to scroll down/up when a table
// in peek mode is full height of peek". A box-drawing table (.peek-box) said
// `touch-action: pan-x`, which forbids a vertical pan that STARTS on it, so a
// table filling the peek left nothing to grab. It must pan sideways itself and
// let a vertical swipe scroll the peek body.
import { test, expect } from './fixtures';

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('amux_walkthrough_done', '1'));
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any)._peekHtml === 'function');
  await page.evaluate(() => {
    eval("peekSession='table-worker'; _peekMsgRowsFor=peekSession; _peekMsgRows=[]; _peekMsgNavKind='all'; _peekMsgIndex=-1; _peekHistoryHTML=''; _peekEarlier={chunks:[],loadedKb:0,done:true,hidden:true,loading:false};");
    const ov = document.getElementById('peek-overlay')!; ov.hidden = false; ov.inert = false;
    ov.setAttribute('aria-hidden', 'false'); ov.classList.add('active');
    (window as any)._stopPeekPoll();
  });
});

async function renderTallTable(page: import('@playwright/test').Page) {
  const row = (i: number) => '│ row ' + String(i).padStart(3, '0') + ' │ ' + 'wide column '.repeat(30) + '│\n';
  let raw = 'before\n'.repeat(30) + '┌' + '─'.repeat(380) + '┐\n';
  for (let i = 0; i < 150; i++) raw += row(i);
  raw += '└' + '─'.repeat(380) + '┘\n' + 'after\n'.repeat(30);
  await page.evaluate(raw => {
    eval('lastPeekHTML=_peekHtml(' + JSON.stringify(raw) + '); _lastLiveHTML=lastPeekHTML;');
    (window as any).applyPeekSearch(false, false);
  }, raw);
  const box = page.locator('.peek-box').first();
  await expect(box).toBeVisible();
  return box;
}

// Every engine: the table lets a vertical pan start on it. This is the rule
// that was wrong (`pan-x` alone), checked where the gesture cannot be driven.
test('a peek table lets vertical pans start on it', async ({ page }) => {
  const box = await renderTallTable(page);
  const ta = await box.evaluate(el => getComputedStyle(el).touchAction);
  expect(ta, 'touch-action on .peek-box').toMatch(/pan-y|auto|manipulation/);
  expect(await box.evaluate(el => getComputedStyle(el).overflowY), 'the box must not become a vertical scroller').toBe('hidden');
});

test('a swipe that starts on a full-height peek table scrolls the peek vertically and the table sideways', async ({ page, browserName }) => {
  test.skip(browserName !== 'chromium', 'touch gestures are synthesized over CDP');
  const box = await renderTallTable(page);
  const cdp = await page.context().newCDPSession(page);
  // CONTROL: the same swipe on plain text above the table. CI's headless Linux
  // Chrome ignored synthesized touch entirely (runs 37175520869, 37177952915:
  // scrollTop never moved), so a red there measured the runner, not the rule.
  // The computed-style test above still runs on it.
  const ctl = await page.evaluate(() => {
    const body = document.getElementById('peek-body')!;
    body.scrollTop = 0;
    const r = body.getBoundingClientRect();
    return { x: r.left + r.width / 2, y: r.top + 60 };
  });
  await cdp.send('Input.synthesizeScrollGesture', { x: Math.round(ctl.x), y: Math.round(ctl.y), xDistance: 0, yDistance: -200, gestureSourceType: 'touch', speed: 1200 });
  await page.waitForTimeout(300);
  const ctlMoved = await page.evaluate(() => document.getElementById('peek-body')!.scrollTop);
  test.skip(ctlMoved < 50, `synthesized touch does not scroll plain text here either (scrollTop ${ctlMoved}); the gesture cannot be measured on this browser`);
  // Bring the table up so it covers the whole visible peek body.
  const geo = await page.evaluate(() => {
    const body = document.getElementById('peek-body')!;
    const b = document.querySelector('.peek-box') as HTMLElement;
    body.scrollTop += b.getBoundingClientRect().top - body.getBoundingClientRect().top + 200;
    const r = body.getBoundingClientRect(), t = b.getBoundingClientRect();
    return { covers: t.top <= r.top && t.bottom >= r.bottom, x: r.left + r.width / 2, y: r.top + r.height / 2, top: body.scrollTop };
  });
  expect(geo.covers, 'the table fills the peek body').toBe(true);
  const swipe = (xDistance: number, yDistance: number) => cdp.send('Input.synthesizeScrollGesture', {
    x: Math.round(geo.x), y: Math.round(geo.y), xDistance, yDistance, gestureSourceType: 'touch', speed: 1200,
  });
  // Finger moves up: content scrolls down.
  await swipe(0, -300);
  await expect.poll(() => page.evaluate(() => document.getElementById('peek-body')!.scrollTop),
    { message: 'a vertical swipe on the table must scroll the peek' }).toBeGreaterThan(geo.top + 100);
  // Sideways still pans the table itself.
  await swipe(-300, 0);
  await expect.poll(() => box.evaluate(el => el.scrollLeft), { message: 'a sideways swipe must pan the table' }).toBeGreaterThan(50);
});
