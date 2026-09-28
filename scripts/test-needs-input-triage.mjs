// AMUX-5286 visual + behaviour check: the needs-input triage sheet, at 390px
// and 1280px.
//
// Proxies the live server (so the rest of the dashboard is real), serves THIS
// checkout's app.js / app.css / index.html, and replaces the triage queue with
// a fixture. EVERY write the sheet can make (owner send, board PATCH, email
// approve/reject, snooze, standing approval, triage log) is intercepted and
// answered locally, so nothing is approved, sent or snoozed on the live server.
//
//   node scripts/test-needs-input-triage.mjs [outdir]
//
// Exits non-zero when an assertion fails; prints the screenshot paths.
import { chromium } from 'playwright';
import { readFileSync, mkdirSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import path from 'node:path';

const here = path.dirname(new URL(import.meta.url).pathname);
const root = process.env.AMUX_ROOT || path.resolve(here, '..');
const stat = path.join(root, 'crates/amux-dashboard/static');
const out = process.argv[2] || path.join(process.env.TMPDIR || '/tmp', 'needs-input-triage-shots');
mkdirSync(out, { recursive: true });
const base = execFileSync('amux', ['url']).toString().trim();
const now = Date.now() / 1000;

const blocked = (name, card, ask, since) => ({
  name, running: true, status: 'waiting', waiting_reason: 'owner', waiting_label: 'Needs input · ' + card,
  provider: 'claude', worker_type: 'coding', lifecycle: 'active', agent_state: 'idle', dir: '/tmp', tags: [],
  preview: '', preview_lines: ['The only thing left is ' + card + '.'], task_name: 'fixture', pinned: true,
  owner_block: { card, ask, since, source: 'goal_loop' },
});
const fixtureSessions = [
  blocked('ni-fixture-parity', 'TP-37', 'Sign in once at the vercel app in the tubescience profile?', now - 7200),
  blocked('ni-fixture-loop', 'GL-9', 'Which of the two retry policies should I ship?', now - 600),
  { ...blocked('ni-fixture-spend', 'NI-1', '', 0), status: 'idle', waiting_reason: '', owner_block: undefined, waiting_label: '' },
];
const fixtureQueue = [
  { key: 'card:TP-37', kind: 'card', card: 'TP-37', worker: 'ni-fixture-parity', status: 'needsyou', ask_type: 'credential',
    question: 'Sign in once at https://semantic-search-tawny.vercel.app in the ethan-tubescience Chrome profile, or reply defaults to skip auth checks?',
    unblocks: 'The parity run resumes and posts the scorecard.', context: 'TP-37 context: parity is 41/42 and the last query needs the signed-in view.',
    category: 'other', rank: 1, since: now - 7200, chips: [{ label: 'Defaults', text: 'Use the defaults. Proceed.' }], standing_category: 'credential' },
  { key: 'card:NI-1', kind: 'card', card: 'NI-1', worker: 'ni-fixture-spend', status: 'needsyou', ask_type: 'budget',
    question: 'Approve about $40 of GPU re-extraction for the demo namespace?', unblocks: 'The re-extraction job runs tonight.',
    context: 'Measured gap: 312 docs missing multimodal embeddings.', category: 'money', rank: 0, since: now - 86400 * 3,
    chips: [], standing_category: 'budget' },
  { key: 'email:apr_fixture00000001', kind: 'email', approval_id: 'apr_fixture00000001', card: '', worker: 'gtm-ticker',
    status: 'pending approval', question: 'Send this email to nathan@example.com? "welcome to Mixpeek"',
    unblocks: 'The email is sent exactly as drafted. Decline discards it; nothing is sent.', context: 'Hi Nathan,\n\nSaw Noctrl just set up Mixpeek. Welcome.',
    email: { to: 'nathan@example.com', cc: '', from: 'ethan@mixpeek.com', subject: 'welcome to Mixpeek', endpoint: 'send' },
    category: 'outbound', rank: 0, since: now - 3600, chips: [], standing_category: 'customer_outbound' },
];

const fail = [];
const check = (cond, msg) => { if (!cond) fail.push(msg); console.log((cond ? 'PASS ' : 'FAIL ') + msg); };

const browser = await chromium.launch(process.env.PW_EXECUTABLE ? { executablePath: process.env.PW_EXECUTABLE } : { channel: 'chrome' });
for (const [label, viewport] of [['390', { width: 390, height: 844 }], ['1280', { width: 1280, height: 800 }]]) {
  const mobile = label === '390';
  const ctx = await browser.newContext({ viewport, ignoreHTTPSErrors: true, serviceWorkers: 'block', isMobile: mobile, hasTouch: mobile });
  const page = await ctx.newPage();
  await page.addInitScript(() => localStorage.setItem('amux_walkthrough_done', '1'));
  if (process.env.NI_DEBUG) page.on('console', m => { if (m.type() === 'error') console.log('console.error: ' + m.text()); });
  if (process.env.NI_DEBUG) page.on('pageerror', e => console.log('pageerror: ' + e.message));
  const writes = [];
  const cards = { 'NI-1': { id: 'NI-1', status: 'needsyou', desc: 'ctx' }, 'TP-37': { id: 'TP-37', status: 'needsyou', desc: 'ctx' }, 'GL-9': { id: 'GL-9', status: 'doing', desc: '' } };
  let queue = fixtureQueue.map(x => ({ ...x }));
  let sess = fixtureSessions;
  // At 1280 the first board PATCH is refused, to prove the refusal is shown verbatim.
  let refuseNextPatch = !mobile;
  await page.route(/\/app\.js(\?.*)?$/, r => r.fulfill({ contentType: 'application/javascript', body: readFileSync(path.join(stat, 'app.js'), 'utf8') }));
  await page.route(/\/app\.css(\?.*)?$/, r => r.fulfill({ contentType: 'text/css', body: readFileSync(path.join(stat, 'app.css'), 'utf8') }));
  await page.route(u => new URL(u).pathname === '/', r => r.fulfill({ contentType: 'text/html', body: readFileSync(path.join(stat, 'index.html'), 'utf8') }));
  await page.route(/\/api\/sessions(\?.*)?$/, async r => {
    if (r.request().method() !== 'GET') return r.fulfill({ status: 403, body: '{}' });
    const res = await r.fetch();
    let list = [];
    try { list = await res.json(); } catch { list = []; }
    // Live lanes blocked on the owner would make the count nondeterministic.
    list = (Array.isArray(list) ? list : []).filter(s => !s.name.startsWith('ni-fixture'))
      .map(s => s.waiting_reason === 'owner' ? { ...s, status: 'idle', waiting_reason: '', owner_block: undefined } : s);
    r.fulfill({ contentType: 'application/json', body: JSON.stringify([...sess, ...list]) });
  });
  await page.route(/\/api\/needs-input(\?.*)?$/, r => r.fulfill({ contentType: 'application/json',
    body: JSON.stringify({ items: queue, count: queue.length, snoozed: [], measured: true, n_considered: queue.length }) }));
  await page.route(/\/api\/needs-input\/(snooze|log)$/, async r => {
    const body = JSON.parse(r.request().postData() || '{}');
    writes.push({ url: new URL(r.request().url()).pathname, body });
    if (r.request().url().endsWith('/snooze')) queue = queue.filter(q => q.key !== body.key);
    r.fulfill({ contentType: 'application/json', body: JSON.stringify({ ok: true, key: body.key, until: body.until }) });
  });
  await page.route(/\/api\/sessions\/[^/]+\/(send|steer|keys)(\?.*)?$/, r => {
    const u = new URL(r.request().url());
    if (r.request().method() === 'GET') {
      const id = u.searchParams.get('msg_id') || '';
      const hit = writes.find(w => w.body && w.body.msg_id === id);
      return r.fulfill({ contentType: 'application/json', body: JSON.stringify(hit ? { accepted: true, msg_id: id, id: 'm1' } : { accepted: false }) });
    }
    if (!u.pathname.includes('/ni-fixture')) { writes.push({ url: u.pathname, body: {}, leaked: true }); return r.fulfill({ status: 403, body: '{}' }); }
    const body = JSON.parse(r.request().postData() || '{}');
    writes.push({ url: new URL(r.request().url()).pathname, body });
    const dup = writes.filter(w => w.body && w.body.msg_id && w.body.msg_id === body.msg_id).length > 1;
    r.fulfill({ contentType: 'application/json', body: JSON.stringify({ ok: true, id: 'm1', submitted: true, deduped: dup }) });
  });
  await page.route(/\/api\/board\/(NI-1|TP-37|GL-9)$/, r => {
    const id = decodeURIComponent(new URL(r.request().url()).pathname.split('/').pop());
    const m = r.request().method();
    if (m === 'GET') return r.fulfill({ contentType: 'application/json', body: JSON.stringify(cards[id] || { id, status: 'todo', desc: '' }) });
    const body = JSON.parse(r.request().postData() || '{}');
    writes.push({ url: '/api/board/' + id, method: m, body });
    if (refuseNextPatch) {
      refuseNextPatch = false;
      return r.fulfill({ status: 400, contentType: 'application/json', body: JSON.stringify({ error: 'cross-lane needsyou move requires authorized_by', why: 'fixture refusal' }) });
    }
    const c = cards[id] || (cards[id] = { id, status: 'todo', desc: '' });
    if (body.status) c.status = body.status;
    if (body.desc_append) c.desc += '\n' + body.desc_append;
    queue = queue.filter(q => q.card !== id || c.status === 'needsyou');
    r.fulfill({ contentType: 'application/json', body: JSON.stringify(c) });
  });
  await page.route(/\/api\/(email\/(approve|reject)\/[^?]*|approvals\/standing)(\?.*)?$/, r => {
    writes.push({ url: new URL(r.request().url()).pathname, body: r.request().postData() });
    const email = r.request().url().includes('/email/');
    if (email) queue = queue.filter(q => q.kind !== 'email');
    r.fulfill({ contentType: 'application/json', body: JSON.stringify(email ? { ok: true, sent_for_session: 'gtm-ticker' } : { id: 'SA-99' }) });
  });

  await page.goto(base + '/', { waitUntil: 'domcontentloaded', timeout: 60000 });
  const pill = page.locator('#needs-input-pill');
  await pill.waitFor({ state: 'visible', timeout: 25000 });
  await page.waitForFunction(() => typeof _niQueue !== 'undefined' && _niQueue && _niQueue.length === 3, null, { timeout: 15000 });
  const pillText = mobile ? await page.locator('#needs-input-pill-count').innerText() : await page.locator('#needs-input-pill-text').innerText();
  // The higher bar (69e7e227): TP-37 (credential), NI-1 (spend) and the email
  // count; GL-9 ("which retry policy") is a call the worker makes, behind Show more.
  check(mobile ? pillText.trim() === '3' : /3 need input/.test(pillText), `${label}: pill counts only what needs the owner (${pillText.trim()})`);
  await pill.click();
  const modal = page.locator('#ni-overlay .ni-modal');
  await modal.waitFor({ timeout: 5000 });
  const counter = page.locator('#ni-counter');
  const question = page.locator('#ni-question');
  check((await counter.innerText()) === '1 of 3', `${label}: opens at 1 of 3`);
  check(/Sign in once/.test(await question.innerText()), `${label}: the worker blocked right now comes first`);
  check(/Show 1 more/.test(await page.locator('#ni-older-toggle').innerText()), `${label}: the rest is one tap away (Show 1 more)`);
  const mb = await modal.boundingBox();
  if (mobile) check(mb && mb.width >= 389 && mb.height >= 800, `${label}: full-screen sheet (${mb && Math.round(mb.width)}x${mb && Math.round(mb.height)})`);
  else check(mb && mb.width <= 640 && mb.x > 200, `${label}: centered modal (${mb && Math.round(mb.width)} wide at x=${mb && Math.round(mb.x)})`);
  for (const sel of ['.ni-approve', '.ni-decline', '.ni-reply-btn', '.ni-always', '.ni-snooze1', '.ni-snooze2', '.ni-open', '.ni-next', '.ni-prev', '.ni-close']) {
    const b = await page.locator('#ni-overlay ' + sel).boundingBox();
    check(b && b.height >= 44 && b.width >= 44 && b.x >= 0 && b.x + b.width <= viewport.width + 1, `${label}: ${sel} is a 44px target in the viewport`);
  }
  await page.screenshot({ path: path.join(out, `triage-1-open-${label}.png`) });

  const step = async dir => {
    if (mobile) {
      await page.evaluate(d => {
        const el = document.getElementById('ni-body');
        const mk = (type, x) => {
          const t = new Touch({ identifier: 1, target: el, clientX: x, clientY: 400 });
          el.dispatchEvent(new TouchEvent(type, { touches: type === 'touchend' ? [] : [t], changedTouches: [t], bubbles: true }));
        };
        mk('touchstart', d > 0 ? 300 : 80); mk('touchend', d > 0 ? 80 : 300);
      }, dir);
    } else await page.keyboard.press(dir > 0 ? 'j' : 'ArrowLeft');
  };
  await step(1);
  check((await counter.innerText()) === '2 of 3' && /nathan@example.com/.test(await question.innerText()), `${label}: ${mobile ? 'swipe' : 'j'} steps to the email approval, newest first (2 of 3)`);
  check(await page.locator('#ni-overlay .ni-always').isDisabled(), `${label}: no standing rule from an email approval`);
  await page.screenshot({ path: path.join(out, `triage-2-email-${label}.png`) });
  await step(1);
  check((await counter.innerText()) === '3 of 3' && /\$40 of GPU/.test(await question.innerText()), `${label}: steps to the spend ask (3 of 3)`);
  await step(-1);
  check((await counter.innerText()) === '2 of 3', `${label}: ${mobile ? 'swipe back' : 'ArrowLeft'} returns to 2 of 3`);
  await step(-1);
  check((await counter.innerText()) === '1 of 3', `${label}: back at 1 of 3`);
  await page.evaluate(() => { _niIdx = _niItems(false).findIndex(x => x.card === 'NI-1'); _niPanel = ''; _niRender(); });

  // APPROVE the money ask.
  await page.locator('#ni-overlay .ni-approve').click();
  await page.waitForTimeout(600);
  if (process.env.NI_DEBUG) console.log('writes after approve: ' + JSON.stringify(writes).slice(0, 800));
  const toast = async () => ((await page.locator('#toast').textContent()) || '').trim();
  const waitToast = async (re, ms = 10000) => { const t0 = Date.now(); let v = ''; while (Date.now() - t0 < ms) { v = await toast(); if (re.test(v)) return v; await page.waitForTimeout(100); } return v; };
  const waitWrite = async (pred, ms = 10000) => { const t0 = Date.now(); while (Date.now() - t0 < ms) { const w = writes.filter(pred); if (w.length) return w; await page.waitForTimeout(150); } return []; };
  if (mobile) {
    const tmsg = await waitToast(/Approved:|Refused:/);
    const send = (await waitWrite(w => w.url === '/api/sessions/ni-fixture-spend/send'))[0];
    const patch = writes.find(w => w.url === '/api/board/NI-1');
    check(send && /^Approved \(NI-1\): Approve about \$40/.test(send.body.text) && /Proceed\.$/.test(send.body.text), `${label}: approve sends the owner message (${send && send.body.text.slice(0, 50)})`);
    check(patch && patch.body.status === 'todo' && /Approved by owner/.test(patch.body.desc_append || ''), `${label}: approve moves NI-1 to todo and appends the decision`);
    check(/Approved: message queued for ni-fixture-spend, NI-1 moved to todo/.test(tmsg), `${label}: toast says what happened (${tmsg})`);
    check(/ of 2$/.test(await counter.innerText()), `${label}: auto-advances, one fewer left (${await counter.innerText()})`);
    const log = writes.find(w => w.url === '/api/needs-input/log' && w.body.action === 'approve');
    check(log && log.body.outcome === 'ok' && log.body.card === 'NI-1', `${label}: triage_action logged server-side`);
  } else {
    const ref = await waitToast(/Refused:/);
    check(/Refused: NI-1: cross-lane needsyou move requires authorized_by/.test(ref), `${label}: a board refusal is shown verbatim (${ref})`);
    const log = writes.find(w => w.url === '/api/needs-input/log' && w.body.action === 'approve');
    check(log && log.body.outcome === 'partial', `${label}: the refusal is logged as partial`);
    check((await counter.innerText()) === '3 of 3', `${label}: a refused item stays put for a retry`);
    // Idempotent retry: same msg_id (server dedups), and the board now accepts.
    await page.locator('#ni-overlay .ni-approve').click();
    await page.waitForTimeout(600);
    const rt = await waitToast(/^Approved:/);
    check(/Approved: message queued for ni-fixture-spend, NI-1 moved to todo/.test(rt), `${label}: retry toast (${rt})`);
    await waitWrite(w => w.url === '/api/sessions/ni-fixture-spend/send' && w.body.text);
    await page.waitForTimeout(1500);
    const sends = writes.filter(w => w.url === '/api/sessions/ni-fixture-spend/send');
    check(sends.length >= 1 && new Set(sends.map(x => x.body.msg_id)).size === 1, `${label}: the retry reuses the msg_id (${sends.length} deliveries, 1 id)`);
    check(/ of 2$/.test(await counter.innerText()), `${label}: auto-advances after the retry lands`);
  }
  await page.screenshot({ path: path.join(out, `triage-3-after-approve-${label}.png`) });

  // REPLY on TP-37, via the "Defaults" chip at 390 and typed text at 1280.
  await page.evaluate(() => { const i = _niItems().findIndex(x => x.card === 'TP-37'); _niIdx = i; _niPanel = ''; _niRender(); });
  await page.locator('#ni-overlay .ni-reply-btn').click();
  const chip = page.locator('#ni-overlay .ni-chip');
  check((await chip.count()) === 1 && (await chip.innerText()) === 'Defaults', `${label}: Defaults chip offered from the ask text`);
  await page.screenshot({ path: path.join(out, `triage-4-reply-${label}.png`) });
  if (mobile) await chip.click();
  else { await page.fill('#ni-reply', 'Skip the sign-in, use the cached token.'); await page.locator('#ni-overlay .ni-send').click(); }
  await page.waitForTimeout(600);
  const rsend = (await waitWrite(w => w.url === '/api/sessions/ni-fixture-parity/send'))[0];
  check(rsend && (mobile ? rsend.body.text === 'Re (TP-37): Use the defaults. Proceed.' : /^Re \(TP-37\): Skip the sign-in/.test(rsend.body.text)), `${label}: reply reaches the worker as the owner (${rsend && rsend.body.text})`);
  const rpatch = writes.filter(w => w.url === '/api/board/TP-37').pop();
  check(rpatch && /Owner reply by owner in needs-input triage|Owner reply/.test(rpatch.body.desc_append || ''), `${label}: reply recorded on TP-37`);

  // SNOOZE whatever is current.
  const before = await counter.innerText();
  const cur = await page.evaluate(() => _niCurrent().item.key);
  await page.locator('#ni-overlay .ni-snooze1').click();
  await page.waitForTimeout(400);
  const sn = writes.find(w => w.url === '/api/needs-input/snooze');
  const tnow = Date.now() / 1000;
  check(sn && sn.body.key === cur && sn.body.until > tnow + 3500 && sn.body.until < tnow + 3700, `${label}: snooze 1h stored server-side for ${cur}`);
  check(!(await page.evaluate(k => _niItems(true).some(i => i.key === k), cur)), `${label}: snoozed item leaves the queue (${cur})`);
  await page.screenshot({ path: path.join(out, `triage-5-after-snooze-${label}.png`) });

  // APPROVE ALL (15:07): two taps, skips what only the owner can do.
  queue = fixtureQueue.map(x => ({ ...x }));
  cards['NI-1'].status = 'needsyou'; cards['TP-37'].status = 'needsyou';
  await page.evaluate(async () => { _niHandled.clear(); await _niFetch(); _niIdx = 0; _niPanel = ''; _niRender(); });
  const before_all = writes.length;
  const allBtn = page.locator('#ni-approve-all');
  check(await allBtn.isVisible() && /Approve all \(2\)/.test(await allBtn.innerText()), `${label}: Approve all offered for the 2 approvable items (${await allBtn.innerText()})`);
  await allBtn.click();
  const conf = await page.locator('#ni-overlay .ni-confirm-all').innerText();
  check(/Approve 2 items\?/.test(conf) && /1 outbound/.test(conf) && /1 spend/.test(conf) && /Leaves 1 that need you/.test(conf), `${label}: first tap only confirms, and says what goes out (${conf.replace(/\s+/g, ' ').slice(0, 120)})`);
  check(writes.length === before_all, `${label}: nothing written before the confirm tap`);
  await page.screenshot({ path: path.join(out, `triage-7-approve-all-confirm-${label}.png`) });
  await page.locator('#ni-overlay .ni-confirm-all .ni-approve').click();
  const doneAll = await waitToast(/^Approved \d+/, 15000);
  const sends_all = writes.slice(before_all).filter(w => w.url === '/api/sessions/ni-fixture-spend/send');
  const email_all = writes.slice(before_all).filter(w => /\/api\/email\/approve/.test(w.url));
  check(/^Approved 2$/.test(doneAll), `${label}: approve all reports its result (${doneAll})`);
  // NI-1 was approved earlier in this run with the same text, so the owner
  // message is deduped client-side and only the card records the decision.
  const ni1_all = writes.slice(before_all).filter(w => w.url === '/api/board/NI-1');
  check((sends_all.length >= 1 || ni1_all.length >= 1) && email_all.length === 1, `${label}: approve all approved the spend and sent the email (sends ${sends_all.length}, NI-1 patches ${ni1_all.length}, email ${email_all.length})`);
  check(/Sign in once/.test(await question.innerText()) && (await counter.innerText()) === '1 of 1', `${label}: the credential ask stays for the owner`);
  await page.screenshot({ path: path.join(out, `triage-8-after-approve-all-${label}.png`) });

  // EMPTY STATE.
  queue = []; sess = [];
  await page.evaluate(() => { _niQueue = []; sessions = sessions.filter(s => !s.name.startsWith('ni-fixture')); _niRender(); updateNeedsInputPill(); });
  check(/Nothing needs you\./.test(await page.locator('#ni-body').innerText()), `${label}: empty state reads "Nothing needs you."`);
  check(!(await pill.isVisible()), `${label}: pill hides at zero`);
  await page.screenshot({ path: path.join(out, `triage-6-empty-${label}.png`) });
  await page.keyboard.press('Escape');
  check(!(await page.locator('#ni-overlay').count()), `${label}: Escape closes`);

  const leaked = writes.filter(w => !/^\/api\/(sessions\/ni-fixture|board\/|needs-input\/|email\/|approvals\/)/.test(w.url));
  check(!leaked.length, `${label}: every triage write was intercepted (${writes.length} writes)`);
  await page.unrouteAll({ behavior: 'ignoreErrors' });
  await ctx.close();
}
await browser.close();
console.log('screenshots in ' + out);
if (fail.length) { console.error(fail.length + ' failed'); process.exit(1); }
