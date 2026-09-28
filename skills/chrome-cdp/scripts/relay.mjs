#!/usr/bin/env node
// cdp relay - hold ONE approved DevTools connection to the real Chrome and
// share it with every cdp.mjs client.
//
// Why (Ethan, 2026-09-28: "make this permanent allow for everything"):
// Chrome 144+ shows "Allow remote debugging?" for every NEW connection made
// through chrome://inspect/#remote-debugging, and offers no flag, policy or
// "remember" option to skip it (ChromeDevTools/chrome-devtools-mcp#825, closed
// not planned). cdp.mjs opened one connection per tab daemon plus one per
// `list`, so every tab a worker touched cost a click. This process makes the
// one connection, keeps it for as long as Chrome runs, and multiplexes: the
// dialog appears once per Chrome launch instead of once per tab.
//
// Protocol: clients speak ordinary CDP to ws://127.0.0.1:<port>/devtools/browser/relay.
// Request ids are rewritten so clients cannot collide; responses go back to the
// client that asked; events carrying a sessionId go to the client that attached
// that session; browser-level events go to every client. A client's sessions
// are detached when it disconnects.
//
// Dependency-free (Node 22+): the server side speaks RFC 6455 by hand, the
// upstream side uses Node's built-in WebSocket.
//
//   node relay.mjs            run in the foreground
//   GET /json/version         what cdp.mjs reads (same shape as Chrome's)
//   GET /status               upstream state, clients, sessions, counters

import http from 'http';
import crypto from 'crypto';
import { readFileSync, writeFileSync, mkdirSync, existsSync, appendFileSync } from 'fs';
import { homedir } from 'os';
import { execFileSync } from 'child_process';
import { resolve } from 'path';

const PORT = Number(process.env.CDP_RELAY_PORT || 9322);
const HOST = '127.0.0.1';
const WS_PATH = '/devtools/browser/relay';
const UPSTREAM_WAIT_MS = 180_000; // the owner may take a while to click Allow
const RUNTIME_DIR = resolve(homedir(), '.cache', 'cdp');
const STATE_FILE = resolve(RUNTIME_DIR, 'relay.json');
const LOG_FILE = resolve(RUNTIME_DIR, 'relay.log');
try { mkdirSync(RUNTIME_DIR, { recursive: true, mode: 0o700 }); } catch {}

function log(msg) {
  const line = `${new Date().toISOString()} ${msg}\n`;
  try { appendFileSync(LOG_FILE, line); } catch {}
  process.stderr.write(line);
}

// The real Chrome's endpoint, from cdp.mjs itself so discovery lives in one
// place. Re-read on every connect: a Chrome restart changes the browser uuid.
function upstreamUrl() {
  // Tests point the relay at a throwaway headless Chrome instead.
  if (process.env.CDP_RELAY_UPSTREAM) return process.env.CDP_RELAY_UPSTREAM;
  const cdp = resolve(new URL('.', import.meta.url).pathname, 'cdp.mjs');
  return execFileSync(process.execPath, [cdp, 'upstream-url'], {
    encoding: 'utf8', timeout: 10000, env: { ...process.env, CDP_NO_RELAY: '1', CDP_PORT: '', AMUX_PROFILE: '' },
  }).trim();
}

// ---------------------------------------------------------------- upstream

const counters = { upstream_connects: 0, clients_total: 0, requests: 0, events: 0 };
let upstream = null;        // WebSocket to Chrome
let upstreamReady = null;   // Promise resolved when open
let upstreamSince = 0;
let seq = 0;
const pending = new Map();  // relay id -> { client, id, method }
const sessions = new Map(); // sessionId -> client
const clients = new Set();

function connectUpstream() {
  if (upstreamReady) return upstreamReady;
  const url = upstreamUrl();
  counters.upstream_connects++;
  log(`upstream: connecting to ${url} (Chrome shows "Allow remote debugging?" once for this connection)`);
  upstreamReady = new Promise((res, rej) => {
    const ws = new WebSocket(url);
    const timer = setTimeout(() => { try { ws.close(); } catch {} rej(new Error('upstream: not approved within 180s')); }, UPSTREAM_WAIT_MS);
    ws.onopen = () => { clearTimeout(timer); upstream = ws; upstreamSince = Date.now(); log('upstream: connected'); res(ws); };
    ws.onerror = (e) => { clearTimeout(timer); rej(new Error('upstream error: ' + (e.message || e.type))); };
    ws.onclose = () => {
      clearTimeout(timer);
      log('upstream: closed; closing every client so they reconnect through a fresh approval');
      upstream = null; upstreamReady = null; upstreamSince = 0;
      pending.clear(); sessions.clear();
      for (const c of [...clients]) c.close(1011, 'upstream closed');
    };
    ws.onmessage = (ev) => onUpstreamMessage(typeof ev.data === 'string' ? ev.data : Buffer.from(ev.data).toString('utf8'));
  });
  upstreamReady.catch(e => { log(e.message); upstreamReady = null; });
  return upstreamReady;
}

function onUpstreamMessage(text) {
  let msg;
  try { msg = JSON.parse(text); } catch { return; }
  if (msg.id !== undefined) {
    const p = pending.get(msg.id);
    if (!p) return;
    pending.delete(msg.id);
    if (p.method === 'Target.attachToTarget' && msg.result?.sessionId) {
      sessions.set(msg.result.sessionId, p.client);
      p.client.sessions.add(msg.result.sessionId);
    }
    if (!p.client.open) return;
    msg.id = p.id;
    p.client.send(JSON.stringify(msg));
    return;
  }
  counters.events++;
  if (msg.method === 'Target.detachedFromTarget' && msg.params?.sessionId) {
    const owner = sessions.get(msg.params.sessionId);
    sessions.delete(msg.params.sessionId);
    owner?.sessions.delete(msg.params.sessionId);
  }
  if (msg.sessionId) {
    const owner = sessions.get(msg.sessionId);
    if (owner?.open) owner.send(text);
    return;
  }
  for (const c of clients) if (c.open) c.send(text);
}

function forward(client, text) {
  let msg;
  try { msg = JSON.parse(text); } catch { return; }
  if (msg.id === undefined || !upstream) return;
  const id = ++seq;
  pending.set(id, { client, id: msg.id, method: msg.method });
  counters.requests++;
  msg.id = id;
  upstream.send(JSON.stringify(msg));
}

// ------------------------------------------------------ RFC 6455 server side

class Client {
  constructor(socket) {
    this.socket = socket;
    this.open = true;
    this.sessions = new Set();
    this.buf = Buffer.alloc(0);
    this.frag = [];
    this.queue = [];
    socket.on('data', d => this.onData(d));
    socket.on('close', () => this.onClose());
    socket.on('error', () => this.onClose());
  }
  onData(d) {
    this.buf = Buffer.concat([this.buf, d]);
    for (;;) {
      if (this.buf.length < 2) return;
      const b0 = this.buf[0], b1 = this.buf[1];
      const fin = (b0 & 0x80) !== 0, op = b0 & 0x0f, masked = (b1 & 0x80) !== 0;
      let len = b1 & 0x7f, off = 2;
      if (len === 126) { if (this.buf.length < 4) return; len = this.buf.readUInt16BE(2); off = 4; }
      else if (len === 127) { if (this.buf.length < 10) return; len = Number(this.buf.readBigUInt64BE(2)); off = 10; }
      const need = off + (masked ? 4 : 0) + len;
      if (this.buf.length < need) return;
      let payload = this.buf.subarray(off + (masked ? 4 : 0), need);
      if (masked) {
        const key = this.buf.subarray(off, off + 4);
        payload = Buffer.from(payload);
        for (let i = 0; i < payload.length; i++) payload[i] ^= key[i & 3];
      }
      this.buf = this.buf.subarray(need);
      if (op === 0x8) { this.close(1000, ''); return; }
      if (op === 0x9) { this.frame(0xa, payload); continue; }
      if (op === 0xa) continue;
      if (op === 0x1 || op === 0x2 || op === 0x0) {
        this.frag.push(payload);
        if (fin) { const text = Buffer.concat(this.frag).toString('utf8'); this.frag = []; this.onMessage(text); }
      }
    }
  }
  onMessage(text) {
    if (upstream) forward(this, text); else this.queue.push(text);
  }
  flush() { for (const t of this.queue.splice(0)) forward(this, t); }
  frame(op, payload) {
    if (!this.open) return;
    const len = payload.length;
    let head;
    if (len < 126) { head = Buffer.from([0x80 | op, len]); }
    else if (len < 65536) { head = Buffer.alloc(4); head[0] = 0x80 | op; head[1] = 126; head.writeUInt16BE(len, 2); }
    else { head = Buffer.alloc(10); head[0] = 0x80 | op; head[1] = 127; head.writeBigUInt64BE(BigInt(len), 2); }
    this.socket.write(Buffer.concat([head, payload]));
  }
  send(text) { this.frame(0x1, Buffer.from(text, 'utf8')); }
  close(code, reason) {
    if (!this.open) return;
    const body = Buffer.alloc(2 + Buffer.byteLength(reason));
    body.writeUInt16BE(code, 0); body.write(reason, 2);
    try { this.frame(0x8, body); } catch {}
    this.open = false;
    try { this.socket.end(); } catch {}
    this.onClose();
  }
  onClose() {
    this.open = false;
    if (!clients.delete(this)) return;
    for (const [id, p] of pending) if (p.client === this) pending.delete(id);
    // Detach this client's sessions so tabs are not left attached to a dead client.
    for (const sid of this.sessions) {
      sessions.delete(sid);
      if (upstream) upstream.send(JSON.stringify({ id: ++seq, method: 'Target.detachFromTarget', params: { sessionId: sid } }));
    }
  }
}

// ---------------------------------------------------------------- server

const server = http.createServer((req, res) => {
  const url = req.url.split('?')[0];
  if (url === '/json/version') {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ Browser: 'amux-cdp-relay', 'Protocol-Version': '1.3', webSocketDebuggerUrl: `ws://${HOST}:${PORT}${WS_PATH}` }));
    return;
  }
  if (url === '/status') {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({
      measured: true, pid: process.pid, port: PORT,
      upstream: upstream ? 'connected' : (upstreamReady ? 'awaiting_approval' : 'disconnected'),
      upstream_since: upstreamSince ? new Date(upstreamSince).toISOString() : null,
      clients: clients.size, sessions: sessions.size, pending: pending.size, ...counters,
    }));
    return;
  }
  res.writeHead(404); res.end();
});

server.on('upgrade', (req, socket) => {
  const key = req.headers['sec-websocket-key'];
  // Loopback only, and no browser page may drive it: a page's WebSocket always sends Origin.
  if (req.url.split('?')[0] !== WS_PATH || !key || req.headers.origin) {
    socket.end('HTTP/1.1 403 Forbidden\r\n\r\n');
    return;
  }
  const accept = crypto.createHash('sha1').update(key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
  socket.write('HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n' +
               `Sec-WebSocket-Accept: ${accept}\r\n\r\n`);
  socket.setNoDelay(true);
  const c = new Client(socket);
  clients.add(c);
  counters.clients_total++;
  connectUpstream().then(() => c.flush(), (e) => c.close(1011, e.message.slice(0, 100)));
});

server.on('error', (e) => {
  if (e.code === 'EADDRINUSE') { log(`port ${PORT} already in use; another relay is running`); process.exit(0); }
  log(`server error: ${e.message}`); process.exit(1);
});

server.listen(PORT, HOST, () => {
  writeFileSync(STATE_FILE, JSON.stringify({ pid: process.pid, port: PORT, started: new Date().toISOString() }));
  log(`relay listening on ${HOST}:${PORT}`);
});
