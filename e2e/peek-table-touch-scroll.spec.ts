// Ethan, 2026-10-03 23:48: "on mobile its hard to scroll down/up when a table
// in peek mode is full height of peek". A box-drawing table (.peek-box) said
// `touch-action: pan-x`, which forbids a vertical pan that STARTS on it, so a
// table filling the peek left nothing to grab. It must pan sideways itself and
// let a vertical swipe scroll the peek body.
import { test, expect } from './fixtures';

test.beforeEach(async ({ page, browserName }) => {
  test.skip(browserName !== 'chromium', 'touch gestures are synthesized over CDP');
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

test('a swipe that starts on a full-height peek table scrolls the peek vertically and the table sideways', async ({ page }) => {
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
  // Bring the table up so it covers the whole visible peek body.
  const geo = await page.evaluate(() => {
    const body = document.getElementById('peek-body')!;
    const b = document.querySelector('.peek-box') as HTMLElement;
    body.scrollTop += b.getBoundingClientRect().top - body.getBoundingClientRect().top + 200;
    const r = body.getBoundingClientRect(), t = b.getBoundingClientRect();
    return { covers: t.top <= r.top && t.bottom >= r.bottom, x: r.left + r.width / 2, y: r.top + r.height / 2, top: body.scrollTop };
  });
  expect(geo.covers, 'the table fills the peek body').toBe(true);
  const cdp = await page.context().newCDPSession(page);
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
