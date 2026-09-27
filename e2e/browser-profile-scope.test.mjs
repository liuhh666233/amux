// AMUX-5307: the Scope tab's browser profile access editor, driven in a real
// Chromium against a FIXTURE. The editor's functions are lifted out of app.js
// verbatim and mounted with app.css; every request is intercepted, so nothing
// touches the live server or its scope files. Writes are captured and asserted.
//
// Run: node --test e2e/browser-profile-scope.test.mjs
// Screenshots go to $SHOT_DIR (default: the OS temp dir).
import {test} from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {chromium} from 'playwright';

const app = fs.readFileSync('crates/amux-dashboard/static/app.js', 'utf8');
const css = fs.readFileSync('crates/amux-dashboard/static/app.css', 'utf8');
const shotDir = process.env.SHOT_DIR || os.tmpdir();

// The editor block: from its banner comment to the next section banner.
const start = app.indexOf('// ── Browser profile access editor (AMUX-5307)');
const end = app.indexOf('// ── Simple tab config', start);
assert.ok(start > 0 && end > start, 'editor block found in app.js');
const snippet = app.slice(start, end);

const PROFILES = ['default', 'consolidated', 'ethan-tubescience', 'dz-local-test',
  'persona-alex-a1', 'persona-alex-a2', 'persona-sam-c3', 'persona-sam-c4', 'netsuite'];

function fixture(allow, deny) {
  const glob = (p, n) => new RegExp('^' + p.replace(/[.+^${}()|[\]\\]/g, '\\$&')
    .replace(/\*/g, '.*').replace(/\?/g, '.') + '$', 'i').test(n);
  const profiles = PROFILES.map(n => {
    if (deny.some(p => glob(p, n))) return {name: n, allowed: false, rule: {key: 'AMUX_BROWSER_PROFILES_DENY', value: deny.find(p => glob(p, n)), scope: 'group:gtm'}};
    if (allow.length) {
      const hit = allow.find(p => glob(p, n));
      return {name: n, allowed: !!hit, rule: {key: 'AMUX_BROWSER_PROFILES_ALLOW', value: hit || allow.join(','), scope: 'group:gtm'}};
    }
    return {name: n, allowed: true, rule: {key: 'default', value: '*', scope: 'default'}};
  });
  return {
    level: 'group', name: 'gtm',
    keys: {allow: 'AMUX_BROWSER_PROFILES_ALLOW', deny: 'AMUX_BROWSER_PROFILES_DENY'},
    set_here: {allow: allow.join(',') || null, deny: deny.join(',') || null},
    profiles, n_allowed: profiles.filter(p => p.allowed).length,
  };
}

async function run(width) {
  // PW_EXECUTABLE: point at an installed Chromium when the bundled one is absent.
  const browser = await chromium.launch(process.env.PW_EXECUTABLE ? {executablePath: process.env.PW_EXECUTABLE} : {});
  const page = await browser.newPage({viewport: {width, height: 900}});
  let state = {allow: ['persona-*'], deny: ['netsuite']};
  const writes = [];
  await page.route('**/api/browser/profile-access**', r =>
    r.fulfill({json: fixture(state.allow, state.deny)}));
  await page.route('**/api/scope', async r => {
    const body = JSON.parse(r.request().postData() || '{}');
    writes.push(body);
    const v = body.value || {};
    const split = s => (s ? String(s).split(',').filter(Boolean) : []);
    state = {allow: split(v.AMUX_BROWSER_PROFILES_ALLOW), deny: split(v.AMUX_BROWSER_PROFILES_DENY)};
    await r.fulfill({json: {ok: true}});
  });
  await page.route('http://fixture.local/', r => r.fulfill({contentType: 'text/html', body:
    `<!doctype html><html data-theme="dark"><head><meta name="viewport" content="width=device-width,initial-scale=1"><style>${css}</style></head>
     <body style="padding:12px;background:var(--bg,#0d1117);color:var(--text,#e6edf3)">
     <div class="scope-bp" id="bp"></div></body></html>`}));
  await page.goto('http://fixture.local/');
  const helpers = String.raw`
    const API = ''; const _authHeaders = () => ({}); window.toasts = [];
    const showToast = (m) => window.toasts.push(m);
    function esc(s){return String(s==null?'':s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');}
    function escJs(s){return String(s).replace(/\\/g,'\\\\').replace(/'/g,"\\'");}
  `;
  await page.addScriptTag({content: helpers + snippet +
    "\nwindow._go = () => _scopeBpLoad('group', 'gtm', 'bp');"});
  await page.evaluate(() => window._go());
  await page.waitForSelector('.scope-bp-prof');

  // Initial render: rules as chips, verdicts on the profile chips.
  assert.equal(await page.locator('.scope-bp-chip.allow').count(), 1);
  assert.equal(await page.locator('.scope-bp-chip.deny').count(), 1);
  assert.match(await page.locator('.scope-bp-count').innerText(), /4 of 9 usable at group gtm/);
  assert.equal(await page.locator('.scope-bp-prof.no').count(), 5);
  await page.screenshot({path: path.join(shotDir, `browser-profile-scope-${width}-initial.png`), fullPage: true});

  // Tap a profile to add it to Allow, switch to Deny and type a glob.
  await page.locator('.scope-bp-prof', {hasText: 'ethan-tubescience'}).click();
  await page.locator('.scope-bp-seg button', {hasText: 'Add to Deny'}).click();
  await page.fill('.scope-bp-input', 'persona-sam-*');
  await page.locator('.scope-bp-add .btn').click();
  assert.match(await page.locator('.scope-bp-msg').innerText(), /Unsaved changes/);
  await page.screenshot({path: path.join(shotDir, `browser-profile-scope-${width}-edited.png`), fullPage: true});

  // Save writes ONE PUT /api/scope env with both keys, then re-reads.
  await page.locator('.scope-bp-save').click();
  await page.waitForFunction(() => document.querySelector('.scope-bp-save')?.disabled === true);
  assert.equal(writes.length, 1);
  assert.deepEqual(writes[0], {level: 'group', name: 'gtm', capability: 'env', value: {
    AMUX_BROWSER_PROFILES_ALLOW: 'persona-*,ethan-tubescience',
    AMUX_BROWSER_PROFILES_DENY: 'netsuite,persona-sam-*'}});
  assert.match(await page.locator('.scope-bp-count').innerText(), /3 of 9 usable/);

  // Removing every chip sends null, which deletes the keys (inherit).
  while (await page.locator('.scope-bp-chip button').count()) {
    await page.locator('.scope-bp-chip button').first().click();
  }
  await page.locator('.scope-bp-save').click();
  await page.waitForFunction(() => document.querySelector('.scope-bp-save')?.disabled === true);
  assert.deepEqual(writes[1].value, {AMUX_BROWSER_PROFILES_ALLOW: null, AMUX_BROWSER_PROFILES_DENY: null});
  assert.match(await page.locator('.scope-bp-count').innerText(), /9 of 9 usable/);

  // No horizontal overflow at this width.
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  assert.ok(overflow <= 0, `no horizontal overflow at ${width}px (got ${overflow})`);
  await page.screenshot({path: path.join(shotDir, `browser-profile-scope-${width}-saved.png`), fullPage: true});
  await browser.close();
}

test('profile access editor at 390px', () => run(390));
test('profile access editor at 1280px', () => run(1280));
