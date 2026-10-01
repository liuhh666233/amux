import {test, expect} from './fixtures';

// Ethan, 2026-09-14: "put the paused accordion immediately above the archived
// accordion". Checked in the real page with the render the dashboard ships: the
// Paused footer must be the element directly before Archived in the sidebar,
// below the worker cards, and visually stacked on top of it at every width.
test('the Paused accordion renders immediately above the Archived accordion, below the worker cards', async ({page}, info) => {
  const workers = [
    {name: 'live-worker', provider: 'claude', running: true, status: 'active', lifecycle: 'active', dir: '/tmp'},
    {name: 'resting-worker', provider: 'claude', running: false, status: 'idle', lifecycle: 'paused', dir: '/tmp'},
    {name: 'old-worker', provider: 'claude', running: false, status: 'idle', lifecycle: 'archived', archived: true, dir: '/tmp'},
  ];
  await page.addInitScript(() => localStorage.setItem('amux_walkthrough_done', '1'));
  await page.route(/\/api\/sessions(?:\?.*)?$/, r => r.fulfill({json: workers}));
  await page.goto('/#view=sessions');
  await page.waitForFunction(() => typeof (window as any).render === 'function');
  await page.evaluate(ws => { eval('sessions=' + JSON.stringify(ws) + '; render();'); }, workers);

  const paused = page.locator('#paused-section .paused-footer');
  const archived = page.locator('#archived-section .archived-footer');
  await expect(paused).toContainText('1 paused');
  await expect(archived).toBeVisible();

  // DOM order: cards, then paused, then archived, with nothing VISIBLE in
  // between.
  //
  // "Nothing visible" rather than "nextElementSibling is archived-section"
  // (AMUX-4869). a2dbd758 added `#expired-section` between them, and
  // `_renderExpiredSection` writes `el.innerHTML = ''` when there are no
  // ephemeral workers to show, which is this fixture. So the requirement Ethan
  // stated is still met on screen while the adjacency check was red, and a
  // guard that fails on an empty structural sibling fails on the refactor
  // rather than on the regression.
  //
  // This is STRICTER than the old check where it matters: if the expired
  // accordion ever renders content here, paused is genuinely no longer
  // immediately above archived, and the walk below says so by name.
  // render() replaces both accordion nodes. Three locator.boundingBox calls
  // can retain a detached node between protocol turns on WebKit, even though
  // the new node is visible. Measure the same rendered frame in one browser
  // turn; never combine geometry from different renders or retry bad geometry.
  const snapshots = await page.evaluate(async () => {
    const samples = [];
    for (let frame = 0; frame < 6; frame++) {
      if (frame) {
        await new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
        (window as any).render();
      }
      const p = document.getElementById('paused-section')!;
      const between: string[] = [];
      for (let n = p.nextElementSibling; n && n.id !== 'archived-section'; n = n.nextElementSibling) {
        const el = n as HTMLElement;
        if (el.offsetHeight > 0 || (el.textContent || '').trim()) between.push(el.id || el.className);
      }
      const box = (selector: string) => {
        const el = document.querySelector(selector);
        if (!el || !el.getClientRects().length) return null;
        const rect = el.getBoundingClientRect();
        return {x: rect.x, y: rect.y, width: rect.width, height: rect.height};
      };
      samples.push({
        frame,
        order: {
          // Grouped worker sections (review, project workers, ...) sit between
          // the cards and Paused; Paused still follows the cards' block.
          afterCards: (() => {
            let n = p.previousElementSibling;
            while (n && n.id !== 'cards' && /-section$/.test(n.id)) n = n.previousElementSibling;
            return n?.id;
          })(),
          archivedFollows: !!p.parentElement?.querySelector('#archived-section'),
          visibleBetween: between,
        },
        paused: box('#paused-section .paused-footer'),
        archived: box('#archived-section .archived-footer'),
        card: box('#cards .card[data-session="live-worker"]'),
        viewport: innerWidth,
      });
    }
    return samples;
  });
  console.log('paused-archived-layout', JSON.stringify({measured: true, n_considered: snapshots.length, snapshots}));
  for (const snapshot of snapshots) {
    const {order, paused: pb, archived: ab, card, viewport} = snapshot;
    expect(order).toEqual({afterCards: 'cards', archivedFollows: true, visibleBetween: []});  // cards [review] paused archived
    for (const [name, rect] of Object.entries({paused: pb, archived: ab, card})) {
      expect(rect, name + ' visible in frame ' + snapshot.frame).not.toBeNull();
      expect(rect!.height, name + ' has height').toBeGreaterThan(0);
      expect(rect!.width, name + ' has width').toBeGreaterThan(0);
    }
    expect(pb!.y + pb!.height).toBeLessThanOrEqual(ab!.y);
    expect(card!.y + card!.height).toBeLessThanOrEqual(pb!.y);
    expect(pb!.x + pb!.width).toBeLessThanOrEqual(viewport + 1);
  }

  await page.screenshot({path: info.outputPath('paused-above-archived.png'), fullPage: true});
});
