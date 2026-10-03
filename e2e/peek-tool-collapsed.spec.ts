// Ethan, 2026-10-03: "make these not expanded by default" (a Bash(...) block in
// peek showing its whole multi-line command). Tool calls start collapsed;
// replies stay open; a toggle sticks. Real render pipeline (_peekHtml).
import { test, expect } from './fixtures';

const RAW = '\x1b[32m⏺\x1b[39m \x1b[1mBash\x1b[22m(cd /tmp && git fetch -q origin main &&\n'
  + '      git log -1)\n'
  + '  ⎿  abc123 fix things\n'
  + '\n'
  + '\x1b[37m⏺\x1b[39m Done. The fix is pushed and\n'
  + '  the tests pass.\n';

test('tool calls start collapsed, replies stay open, and a toggle sticks', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('amux_walkthrough_done', '1'));
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any)._peekHtml === 'function');
  // Rendered into a probe that stays in the page, as #peek-body does, so the
  // toggle can see the element it flips.
  const state = () => page.evaluate((raw) => {
    let body = document.getElementById('ptc-probe');
    if (!body) { body = document.createElement('div'); body.id = 'ptc-probe'; document.body.appendChild(body); }
    body.innerHTML = (window as any)._peekHtml(raw);
    return [...body.querySelectorAll('.ptc')].map(el => ({ head: (el.querySelector('.ptc-head')!.textContent || '').trim().slice(0, 12), collapsed: el.classList.contains('collapsed') }));
  }, RAW);
  const first = await state();
  expect(first).toEqual([{ head: expect.stringContaining('Bash('), collapsed: true }, { head: expect.stringContaining('Done.'), collapsed: false }]);
  // Toggle the tool call open, then re-render: the reader's choice holds.
  await page.evaluate(() => { (window as any)._peekToggleToolNow(0); });
  expect((await state())[0].collapsed).toBe(false);
});
