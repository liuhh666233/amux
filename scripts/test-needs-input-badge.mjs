// AMUX-5277 visual check: the "Needs input · TP-37" badge, the peek header
// badge and the header pill, at 390px and 1280px.
//
// Proxies the live server (so the rest of the fleet list is real) and serves
// THIS checkout's app.js / app.css / index.html, then injects one fixture
// worker with the new session-list fields. Nothing is written to the server.
//
//   node scripts/test-needs-input-badge.mjs [outdir]
//
// Exits non-zero when an assertion fails; prints the screenshot paths.
import { chromium } from 'playwright';
import { readFileSync, mkdirSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import path from 'node:path';

const here = path.dirname(new URL(import.meta.url).pathname);
const root = process.env.AMUX_ROOT || path.resolve(here, '..');
const stat = path.join(root, 'crates/amux-dashboard/static');
const out = process.argv[2] || path.join(process.env.TMPDIR || '/tmp', 'needs-input-shots');
mkdirSync(out, { recursive: true });
const base = execFileSync('amux', ['url']).toString().trim();

const fixture = {
  name: 'tubescience-parity', running: true, status: 'waiting', waiting_reason: 'owner',
  waiting_label: 'Needs input · TP-37', provider: 'claude', worker_type: 'coding', lifecycle: 'active',
  agent_state: 'idle', dir: '/Users/ethan/Dev/mixpeek/customers/tubescience', tags: [], preview: '', preview_lines: [],
  task_name: 'TubeScience search parity', model: 'claude-opus-5', pinned: true,
  owner_block: { card: 'TP-37', ask: 'Sign in once at https://semantic-search-tawny.vercel.app in the ethan-tubescience Chrome profile', since: 1790520000, source: 'goal_loop' },
};

const fail = [];
const check = (cond, msg) => { if (!cond) fail.push(msg); console.log((cond ? 'PASS ' : 'FAIL ') + msg); };

const browser = await chromium.launch(process.env.PW_EXECUTABLE ? { executablePath: process.env.PW_EXECUTABLE } : {});
for (const [label, viewport] of [['390', { width: 390, height: 844 }], ['1280', { width: 1280, height: 900 }]]) {
  const ctx = await browser.newContext({ viewport, ignoreHTTPSErrors: true, serviceWorkers: 'block', isMobile: label === '390', hasTouch: label === '390' });
  const page = await ctx.newPage();
  await page.addInitScript(() => localStorage.setItem('amux_walkthrough_done', '1'));
  await page.route(/\/app\.js(\?.*)?$/, r => r.fulfill({ contentType: 'application/javascript', body: readFileSync(path.join(stat, 'app.js'), 'utf8') }));
  await page.route(/\/app\.css(\?.*)?$/, r => r.fulfill({ contentType: 'text/css', body: readFileSync(path.join(stat, 'app.css'), 'utf8') }));
  await page.route(u => new URL(u).pathname === '/', r => r.fulfill({ contentType: 'text/html', body: readFileSync(path.join(stat, 'index.html'), 'utf8') }));
  await page.route(/\/api\/sessions(\?.*)?$/, async r => {
    const res = await r.fetch();
    let list = [];
    try { list = await res.json(); } catch { list = []; }
    list = (Array.isArray(list) ? list : []).filter(s => s.name !== fixture.name);
    r.fulfill({ contentType: 'application/json', body: JSON.stringify([fixture, ...list]) });
  });
  const opened = [];
  await page.exposeFunction('__recordOpen', id => opened.push(id));
  await page.goto(base + '/');
  await page.evaluate(() => { const o = window.openBoardDetail; window.openBoardDetail = id => { window.__recordOpen(id); }; window.__origOpen = o; });
  const badge = page.locator('.status-badge.needs-input').first();
  await badge.waitFor({ timeout: 20000 });
  await badge.scrollIntoViewIfNeeded();
  const text = (await badge.innerText()).replace(/\s+/g, ' ');
  check(/needs input · TP-37/i.test(text), `${label}: card badge reads "${text.slice(0, 60)}"`);
  check(/sign in once/i.test(text), `${label}: card badge carries the one-line ask`);
  const box = await badge.boundingBox();
  check(box && box.height >= 44, `${label}: card badge is a 44px target (${box && box.height})`);
  check(box && box.x >= 0 && box.x + box.width <= viewport.width + 1, `${label}: card badge fits the viewport`);
  const bg = await badge.evaluate(e => getComputedStyle(e).backgroundColor);
  const working = await page.evaluate(() => { const d = document.createElement('span'); d.className = 'status-badge active'; document.body.appendChild(d); const c = getComputedStyle(d).backgroundColor; d.remove(); return c; });
  const waiting = await page.evaluate(() => { const d = document.createElement('span'); d.className = 'status-badge waiting'; document.body.appendChild(d); const c = getComputedStyle(d).backgroundColor; d.remove(); return c; });
  check(bg !== working && bg !== waiting, `${label}: own colour (${bg}) distinct from working (${working}) and waiting (${waiting})`);
  const pill = page.locator('#needs-input-pill');
  check(await pill.isVisible(), `${label}: header pill visible`);
  const pillBox = await pill.boundingBox();
  check(pillBox && pillBox.height >= 44 && pillBox.width >= 44, `${label}: header pill is a 44px target`);
  check(pillBox && pillBox.x + pillBox.width <= viewport.width + 1, `${label}: header pill fits the viewport`);
  const pillText = label === '390' ? await page.locator('#needs-input-pill-count').innerText() : await page.locator('#needs-input-pill-text').innerText();
  check(label === '390' ? pillText.trim() === '1' : /1 needs input/.test(pillText), `${label}: pill reads "${pillText.trim()}"`);
  await page.screenshot({ path: path.join(out, `needs-input-list-${label}.png`) });
  await badge.click();
  check(opened.includes('TP-37'), `${label}: tapping the badge opens TP-37`);
  // The pill jumps to the worker: the peek header shows the same badge.
  await pill.click();
  const peekBadge = page.locator('#peek-session-status .status-badge.needs-input');
  await peekBadge.waitFor({ timeout: 15000 });
  const pb = await peekBadge.boundingBox();
  check((await peekBadge.innerText()).includes('TP-37'), `${label}: peek header badge names TP-37`);
  check(pb && pb.height >= 44 && pb.x + pb.width <= viewport.width + 1, `${label}: peek header badge is a 44px target inside the viewport`);
  await page.screenshot({ path: path.join(out, `needs-input-peek-${label}.png`) });
  await page.unrouteAll({ behavior: 'ignoreErrors' });
  await ctx.close();
}
await browser.close();
console.log('screenshots in ' + out);
if (fail.length) { console.error(fail.length + ' failed'); process.exit(1); }
