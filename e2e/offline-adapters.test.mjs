import {test} from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

// Offline copies of workers are driven by one table, _OFFLINE_ADAPTERS, keyed
// by renderer (2026-09-27). Prefetch, the cap and eviction loop over it. Before
// the table there were three hand-written lists of key prefixes; the post-sync
// prune missed `file_` while the cap trim had it, which is the leak this shape
// exists to prevent. These tests run the SHIPPED slice of app.js in a sandbox
// with an in-memory IndexedDB and a scripted server.

const source = fs.readFileSync('crates/amux-dashboard/static/app.js', 'utf8');
const start = source.indexOf('const _PEEK_CACHE_LINES');
const endMark = 'async function _chatCacheGet';
const end = source.indexOf('\n}\n', source.indexOf(endMark)) + 3;
assert.ok(start > 0 && end > start, 'offline slice markers must exist in app.js');
const code = source.slice(start, end);

function fixture(sessions, server) {
  const store = new Map();
  const toasts = [];
  const calls = [];
  const ctx = vm.createContext({
    console, Date, Math, JSON, Promise, Object, Array, Set, Map, parseInt, encodeURIComponent,
    navigator: {}, API: '', sessions, peekSession: '', _lastPeekedSession: '',
    online: true, showToast: t => toasts.push(t), render() {}, _offlineInfoRefresh() {},
    _authHeaders: () => ({}), hidePeekLoading() {}, _chatRender() {}, _chatTime: () => '',
    document: { getElementById: () => null },
    _idb: {
      get: async k => store.get(k), set: async (k, v) => { store.set(k, v); }, del: async k => { store.delete(k); },
    },
    fetch: async (url, opts = {}) => {
      calls.push(url);
      if (url.includes('/api/worker-types')) return { ok: false, json: async () => ({}) };
      const reply = server(url, opts);
      return {
        ok: reply.status === 200, status: reply.status,
        headers: { get: h => (h === 'ETag' ? reply.etag || '' : '') },
        json: async () => reply.body,
      };
    },
  });
  vm.runInContext(code, ctx);
  return { ctx, store, toasts, calls, run: src => vm.runInContext(src, ctx) };
}

const SESSIONS = () => [
  { name: 'code-a', running: true, status: 'active', worker_type: 'coding' },
  { name: 'chat-b', running: true, status: 'idle', worker_type: 'chat' },
  { name: 'other-c', running: true, status: 'idle', renderer: 'browser' },
];
function server({ etag = 'e1', messages = [{ role: 'user', text: 'hi' }] } = {}) {
  return (url, opts) => {
    if (url.includes('/peek?')) {
      if (opts.headers && opts.headers['If-None-Match'] === etag) return { status: 304 };
      return { status: 200, etag, body: { output: 'frame', history: 'line1\nline2\n' } };
    }
    if (url.includes('/chat?')) return { status: 200, body: { messages } };
    return { status: 404 };
  };
}

test('prefetch saves each worker type under its adapter key and skips types with no adapter', async () => {
  const f = fixture(SESSIONS(), server());
  await f.ctx._offlinePrefetch(true);
  assert.deepEqual(f.store.get('peek_code-a').history, 'line1\nline2\n');
  assert.equal(f.store.get('chat_chat-b').messages.length, 1);
  assert.equal(f.store.has('peek_other-c') || f.store.has('chat_other-c'), false);
  assert.equal(f.calls.some(u => u.includes('other-c')), false, 'no fetch for a renderer without an adapter');
  const index = f.store.get('peek_index');
  assert.deepEqual(Object.keys(index).sort(), ['chat-b', 'code-a']);
  assert.match(f.toasts.at(-1), /Offline ready: 2 workers · \d+KB new/);
});

test('an unchanged worker of either type counts as unchanged on the next pass', async () => {
  const f = fixture(SESSIONS(), server());
  await f.ctx._offlinePrefetch(true);
  await f.ctx._offlinePrefetch(true);
  assert.equal(f.toasts.at(-1), 'Offline ready: 2 workers · already current');
});

test('a stopped worker loses every type of offline copy, including legacy keys', async () => {
  const sessions = SESSIONS();
  const f = fixture(sessions, server());
  await f.ctx._offlinePrefetch(true);
  f.store.set('file_code-a', { legacy: true });
  f.store.set('chat_code-a', { stray: true });
  sessions[0].running = false;
  await f.ctx._offlinePrefetch(true);
  for (const k of ['peek_code-a', 'chat_code-a', 'file_code-a']) assert.equal(f.store.has(k), false, k);
  assert.ok(f.store.has('chat_chat-b'));
  assert.deepEqual(Object.keys(f.store.get('peek_index')), ['chat-b']);
});

test('the cap evicts the oldest worker regardless of its type', async () => {
  const f = fixture(SESSIONS(), server());
  await f.ctx._offlinePrefetch(true);
  f.run("_peekIndex['code-a'].time = 1; _peekIndex['chat-b'].time = 2; _offlineCap = 1;");
  await f.run('_peekIndexSave(); _offlineTrimToCap()');
  assert.equal(f.store.has('peek_code-a'), false);
  assert.ok(f.store.has('chat_chat-b'));
});

test('no offline key prefix is written by hand outside the adapter table', () => {
  const tableStart = source.indexOf('const _PEEK_CACHE_KEY');
  const tableEnd = source.indexOf('function _offlineDropWorker');
  const outside = source.slice(0, tableStart) + source.slice(tableEnd);
  const literal = outside.match(/'(?:peek|chat|file)_' \+/g) || [];
  assert.deepEqual(literal, [], 'use the adapter key functions so eviction and the cap see every copy');
});
