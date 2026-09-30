"""The ChatGPT front door, end to end, with no cloud host (AMUX-5397).

Runs the REAL gateway.Handler on loopback against fake workspaces that follow
the workspace contract in crates/amux-server/src/api/chatgpt_app.rs:
register, authorize (JSON), owner approve, status poll that mints its code
once, token (code + refresh), and a token-checked /mcp. One fake sits behind a
fake tunnel client that long-polls the gateway the way
runtime_jobs/tunnel.rs does, stamping X-Amux-Tunnel-Relay.

    python3 cloud/tests/test_chatgpt_front.py

No Clerk, Docker, Stripe or network: the gateway's env is dummy values and
its session-stopping hooks are replaced before anything can call them.
"""

import base64
import hashlib
import json
import os
import re
import secrets
import sys
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
GW_DIR = os.path.join(HERE, "..", "gateway")
TMP = tempfile.mkdtemp(prefix="front-door-test-")
os.environ.update({
    "CLERK_PUBLISHABLE_KEY": "pk_test_x", "CLERK_SECRET_KEY": "sk_test_x",
    "R2_ACCESS_KEY": "x", "R2_SECRET_KEY": "x", "CF_ACCOUNT_ID": "x",
    "COOKIE_SECRET": "front-door-test-secret", "GATEWAY_DB": os.path.join(TMP, "gw.db"),
    "AMUX_CLOUD_DATA": os.path.join(TMP, "users"), "CONTAINER_SCHEME": "http",
    "AMUX_FRONT_DOOR_ORIGIN": "https://cloud.amux.io",
})
sys.path.insert(0, GW_DIR)
import gateway  # noqa: E402

gateway._stop_org_sessions = lambda port: []          # never reachable here, and never allowed to act
gateway.stop_container = lambda uid: None

CHATGPT_CB = "https://chatgpt.com/connector_platform_oauth_redirect"
FRONT_CB = "https://cloud.amux.io/oauth/workspace/callback"


def s256(v):
    return base64.urlsafe_b64encode(hashlib.sha256(v.encode()).digest()).rstrip(b"=").decode()


# ── a fake workspace (the Rust contract, reduced) ───────────────────────────
class FakeWorkspace:
    def __init__(self, name):
        self.name = name
        self.clients, self.requests, self.tokens = {}, {}, {}
        self.calls = []
        ws = self

        class H(BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def reply(self, code, obj):
                b = json.dumps(obj).encode()
                self.send_response(code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(b)))
                self.end_headers()
                self.wfile.write(b)

            def body(self):
                n = int(self.headers.get("Content-Length", 0) or 0)
                return self.rfile.read(n) if n else b""

            def do_GET(self):
                u = urllib.parse.urlparse(self.path)
                q = {k: v[0] for k, v in urllib.parse.parse_qs(u.query).items()}
                if u.path == "/oauth/authorize":
                    c = ws.clients.get(q.get("client_id"))
                    if not c or q.get("redirect_uri") not in c or "application/json" not in self.headers.get("Accept", ""):
                        return self.reply(400, {"error": "bad"})
                    rid, k = secrets.token_hex(6), secrets.token_hex(12)
                    ws.requests[rid] = {"k": k, "status": "pending", "cid": q["client_id"], "ruri": q["redirect_uri"],
                                        "ch": q["code_challenge"], "state": q.get("state", ""), "code": None}
                    return self.reply(200, {"status": "pending", "request": rid, "user_code": "AB12-CD34",
                                            "poll": f"oauth/authorize/status?id={rid}&k={k}"})
                if u.path == "/oauth/authorize/status":
                    r = ws.requests.get(q.get("id"))
                    if not r or r["k"] != q.get("k"):
                        return self.reply(200, {"status": "unknown"})
                    if r["status"] == "approved" and r["code"] is None:
                        r["code"] = secrets.token_hex(8)
                        sep = "&" if "?" in r["ruri"] else "?"
                        return self.reply(200, {"status": "approved", "redirect": f"{r['ruri']}{sep}code={r['code']}&state={r['state']}"})
                    if r["status"] == "approved":
                        return self.reply(200, {"status": "expired"})
                    return self.reply(200, {"status": r["status"]})
                self.reply(404, {"error": "nope"})

            def do_POST(self):
                u = urllib.parse.urlparse(self.path)
                raw = self.body()
                if u.path == "/oauth/register":
                    v = json.loads(raw)
                    if v["redirect_uris"] != [FRONT_CB]:
                        return self.reply(400, {"error": "invalid_redirect_uri"})
                    cid = "c" + secrets.token_hex(4)
                    ws.clients[cid] = v["redirect_uris"]
                    return self.reply(201, {"client_id": cid})
                m = re.match(r"^/api/chatgpt-app/requests/([^/]+)/approve$", u.path)
                if m:
                    # The workspace's owner rule: a relayed request never approves.
                    if self.headers.get("X-Amux-Tunnel-Relay") or self.headers.get("X-Forwarded-For"):
                        return self.reply(403, {"why": "relayed_without_owner_credential"})
                    ws.requests[m.group(1)]["status"] = "approved"
                    return self.reply(200, {"ok": True})
                if u.path == "/oauth/token":
                    f = {k: v[0] for k, v in urllib.parse.parse_qs(raw.decode()).items()}
                    if f["grant_type"] == "authorization_code":
                        r = next((r for r in ws.requests.values() if r["code"] and r["code"] == f.get("code")), None)
                        if not r or r.get("used") or s256(f.get("code_verifier", "")) != r["ch"] or f.get("redirect_uri") != r["ruri"]:
                            return self.reply(400, {"error": "invalid_grant"})
                        r["used"] = True
                    elif f["grant_type"] == "refresh_token":
                        t = ws.tokens.pop(f.get("refresh_token"), None)
                        if not t or t != "refresh":
                            return self.reply(400, {"error": "invalid_grant"})
                    at, rt = "wat_" + secrets.token_hex(8), "wrt_" + secrets.token_hex(8)
                    ws.tokens[at], ws.tokens[rt] = "access", "refresh"
                    return self.reply(200, {"access_token": at, "refresh_token": rt, "token_type": "Bearer"})
                if u.path == "/mcp":
                    tok = self.headers.get("Authorization", "")[7:]
                    if ws.tokens.get(tok) != "access":
                        return self.reply(401, {"error": "unauthorized"})
                    req = json.loads(raw)
                    ws.calls.append(req.get("method"))
                    if req.get("method") == "tools/call":
                        return self.reply(200, {"jsonrpc": "2.0", "id": req["id"], "result": {
                            "content": [{"type": "text", "text": f"workspace={ws.name} tool={req['params']['name']}"}]}})
                    return self.reply(200, {"jsonrpc": "2.0", "id": req["id"], "result": {"workspace": ws.name, "tools": [{"name": "list_workers"}]}})
                self.reply(404, {"error": "nope"})

        self.srv = ThreadingHTTPServer(("127.0.0.1", 0), H)
        self.port = self.srv.server_address[1]
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()

    def revoke_all(self):
        self.tokens.clear()


# ── a fake local tunnel client (runtime_jobs/tunnel.rs, reduced) ────────────
MCP_PATHS = {"/mcp", "/oauth/register", "/oauth/authorize", "/oauth/authorize/status", "/oauth/token"}


class FakeTunnel:
    def __init__(self, gw, token, target_port):
        self.gw, self.token, self.target = gw, token, target_port
        reg = gw.call("POST", "/tunnel/register", headers={"Authorization": "Bearer " + token})[1]
        self.tid = reg["tid"]
        self.stop = False
        threading.Thread(target=self.loop, daemon=True).start()

    def loop(self):
        while not self.stop:
            st, item = self.gw.call("GET", f"/tunnel/poll?tid={self.tid}", headers={"Authorization": "Bearer " + self.token})
            if st != 200 or not isinstance(item, dict) or item.get("idle"):
                continue
            if item["path"] not in MCP_PATHS:
                resp = {"status": 404, "headers": {}, "body": ""}
            else:
                hs = {k: v for k, v in item["headers"].items() if k.lower() not in ("x-amux-tunnel-relay", "x-amux-public-base")}
                hs["X-Amux-Tunnel-Relay"] = "1"
                url = f"http://127.0.0.1:{self.target}{item['path']}" + (f"?{item['qs']}" if item["qs"] else "")
                body = base64.b64decode(item["body"]) if item["body"] else None
                r = urllib.request.Request(url, data=body, method=item["method"], headers=hs)
                try:
                    with urllib.request.urlopen(r, timeout=10) as x:
                        resp = {"status": x.status, "headers": dict(x.headers.items()), "body": base64.b64encode(x.read()).decode()}
                except urllib.error.HTTPError as e:
                    resp = {"status": e.code, "headers": dict(e.headers.items()), "body": base64.b64encode(e.read()).decode()}
            self.gw.call("POST", f"/tunnel/reply?rid={item['rid']}", body=resp, headers={"Authorization": "Bearer " + self.token})


class Gateway:
    def __init__(self):
        self.srv = ThreadingHTTPServer(("127.0.0.1", 0), gateway.Handler)
        self.base = f"http://127.0.0.1:{self.srv.server_address[1]}"
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()

    def call(self, method, path, body=None, headers=None, form=None, raw=False):
        hs = dict(headers or {})
        data = None
        if form is not None:
            data = urllib.parse.urlencode(form).encode()
            hs["Content-Type"] = "application/x-www-form-urlencoded"
        elif body is not None:
            data = json.dumps(body).encode()
            hs["Content-Type"] = "application/json"
        req = urllib.request.Request(self.base + path, data=data, method=method, headers=hs)
        opener = urllib.request.build_opener(NoRedirect)
        try:
            r = opener.open(req, timeout=40)
            st, h, b = r.status, dict(r.headers.items()), r.read()
        except urllib.error.HTTPError as e:
            st, h, b = e.code, dict(e.headers.items()), e.read()
        if raw:
            return st, h, b
        try:
            return st, json.loads(b)
        except ValueError:
            return st, b.decode(errors="replace")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **k):
        return None


def cookie(uid):
    return {"Cookie": "amux_session=" + gateway._make_cookie(uid)}


class FrontDoorTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.gw = Gateway()
        cls.ws_a, cls.ws_b, cls.machine_a = FakeWorkspace("A"), FakeWorkspace("B"), FakeWorkspace("machine-A")
        db = gateway.get_db()
        now = int(time.time())
        for org, port, owner in (("orgA", cls.ws_a.port, "alice"), ("orgB", cls.ws_b.port, "bob")):
            # plan pro + no budget: the budget poller must never pick these up.
            db.execute("INSERT INTO orgs (id, name, owner_id, port, plan, created_at) VALUES (?,?,?,?,?,?)",
                       (org, org, owner, port, "pro", now))
            db.execute("INSERT INTO org_memberships VALUES (?,?,?,?)", (org, owner, "owner", now))
        db.execute("INSERT INTO org_memberships VALUES (?,?,?,?)", ("orgA", "mallory", "member", now))
        for uid in ("alice", "bob", "mallory"):
            db.execute("INSERT OR IGNORE INTO users (id, email, plan, created_at, last_seen) VALUES (?,?,?,?,?)",
                       (uid, uid + "@x.test", "pro", now, now))
        db.execute("INSERT INTO tunnel_tokens (token, org_id, label, created_at) VALUES (?,?,?,?)",
                   ("tokA", "orgA", "alice-laptop", now))
        db.execute("INSERT INTO tunnel_tokens (token, org_id, label, created_at) VALUES (?,?,?,?)",
                   ("tokB", "orgB", "bob-laptop", now))
        db.commit()
        cls.tunnel = FakeTunnel(cls.gw, "tokA", cls.machine_a.port)

    # helpers
    def register(self):
        st, reg = self.gw.call("POST", "/oauth/register", body={"client_name": "ChatGPT", "redirect_uris": [CHATGPT_CB]})
        self.assertEqual(st, 201, reg)
        return reg["client_id"]

    def picker(self, user, cid, verifier="v" * 50):
        q = urllib.parse.urlencode({"response_type": "code", "client_id": cid, "redirect_uri": CHATGPT_CB,
                                    "code_challenge": s256(verifier), "code_challenge_method": "S256",
                                    "state": "st1", "scope": "amux:read amux:write",
                                    "resource": "https://cloud.amux.io/mcp"})
        st, page = self.gw.call("GET", "/oauth/authorize?" + q, headers=cookie(user))
        self.assertEqual(st, 200, page)
        req = re.search(r"name=req value='([^']+)'", page).group(1)
        k = re.search(r"name=k value='([^']+)'", page).group(1)
        return page, req, k

    def connect(self, user, ws_value, approve=None):
        cid = self.register()
        verifier = secrets.token_urlsafe(40)
        page, req, k = self.picker(user, cid, verifier)
        self.assertIn(ws_value, page)
        st, waiting = self.gw.call("POST", "/oauth/authorize/choose", form={"req": req, "k": k, "ws": ws_value},
                                   headers=cookie(user))
        self.assertEqual(st, 200, waiting)
        if approve:
            approve()
        for _ in range(20):
            st, w = self.gw.call("GET", f"/oauth/authorize/wait?req={req}&k={k}")
            if w.get("redirect"):
                break
            time.sleep(0.2)
        q = urllib.parse.parse_qs(urllib.parse.urlparse(w["redirect"]).query)
        self.assertEqual(q["state"], ["st1"])
        self.assertEqual(q["iss"], ["https://cloud.amux.io"])
        st, tok = self.gw.call("POST", "/oauth/token", form={
            "grant_type": "authorization_code", "code": q["code"][0], "client_id": cid,
            "redirect_uri": CHATGPT_CB, "code_verifier": verifier, "resource": "https://cloud.amux.io/mcp"})
        self.assertEqual(st, 200, tok)
        return cid, tok, q["code"][0], verifier

    def rpc(self, token, method, params=None):
        return self.gw.call("POST", "/mcp", body={"jsonrpc": "2.0", "id": 7, "method": method, "params": params or {}},
                            headers={"Authorization": "Bearer " + token})

    # tests
    def test_metadata_names_one_origin(self):
        st, pr = self.gw.call("GET", "/.well-known/oauth-protected-resource")
        self.assertEqual(pr["resource"], "https://cloud.amux.io/mcp")
        st, asm = self.gw.call("GET", "/.well-known/oauth-authorization-server")
        self.assertEqual(asm["issuer"], "https://cloud.amux.io")
        self.assertEqual(asm["code_challenge_methods_supported"], ["S256"])

    def test_mcp_without_a_token_points_chatgpt_at_discovery(self):
        st, h, _ = self.gw.call("POST", "/mcp", body={"jsonrpc": "2.0", "id": 1, "method": "tools/list"}, raw=True)
        self.assertEqual(st, 401)
        self.assertIn('resource_metadata="https://cloud.amux.io/.well-known/oauth-protected-resource"', h["WWW-Authenticate"])
        st, _ = self.rpc("fat_bogus", "tools/list")
        self.assertEqual(st, 401)

    def test_cloud_workspace_full_flow(self):
        _, tok, _, _ = self.connect("alice", "container:orgA")
        st, r = self.rpc(tok["access_token"], "tools/list")
        self.assertEqual((st, r["result"]["workspace"]), (200, "A"))
        st, r = self.rpc(tok["access_token"], "tools/call", {"name": "list_workers", "arguments": {}})
        self.assertEqual(r["result"]["content"][0]["text"], "workspace=A tool=list_workers")

    def test_machine_through_tunnel_waits_for_the_local_owner(self):
        ws_value = f"tunnel:{self.tunnel.tid}"
        pending_seen = []

        def owner_approves_on_the_machine():
            # The code is pending until the machine's owner approves it there.
            rid = next(iter(k for k, r in self.machine_a.requests.items() if r["status"] == "pending"))
            pending_seen.append(rid)
            self.machine_a.requests[rid]["status"] = "approved"
        _, tok, _, _ = self.connect("alice", ws_value, approve=owner_approves_on_the_machine)
        self.assertEqual(len(pending_seen), 1)
        st, r = self.rpc(tok["access_token"], "tools/call", {"name": "read_worker", "arguments": {}})
        self.assertEqual(r["result"]["content"][0]["text"], "workspace=machine-A tool=read_worker")

    def test_a_workspace_of_another_org_is_never_offered_nor_accepted(self):
        cid = self.register()
        page, req, k = self.picker("alice", cid)
        self.assertNotIn("container:orgB", page)
        st, _ = self.gw.call("POST", "/oauth/authorize/choose", form={"req": req, "k": k, "ws": "container:orgB"},
                             headers=cookie("alice"))
        self.assertEqual(st, 403, "a forged pick of another tenant's workspace is refused")
        self.assertEqual(self.ws_b.clients, {}, "and workspace B was never contacted")

    def test_someone_elses_request_cannot_be_continued(self):
        cid = self.register()
        _, req, k = self.picker("alice", cid)
        st, _ = self.gw.call("POST", "/oauth/authorize/choose", form={"req": req, "k": k, "ws": "container:orgB"},
                             headers=cookie("bob"))
        self.assertEqual(st, 400, "bob cannot choose on alice's request, even for his own workspace")

    def test_a_plain_member_cannot_connect_a_cloud_workspace(self):
        cid = self.register()
        _, req, k = self.picker("mallory", cid)
        st, page = self.gw.call("POST", "/oauth/authorize/choose", form={"req": req, "k": k, "ws": "container:orgA"},
                                headers=cookie("mallory"))
        self.assertEqual(st, 502)
        self.assertIn("owner or admin", page)

    def test_a_tunnel_that_changes_hands_is_never_re_routed(self):
        _, tok, _, _ = self.connect("alice", f"tunnel:{self.tunnel.tid}", approve=lambda: [
            r.update(status="approved") for r in self.machine_a.requests.values() if r["status"] == "pending"])
        before = len(self.machine_a.calls)
        with gateway._tunnel_lock:
            gateway._tunnels[self.tunnel.tid]["org_id"] = "orgB"
        try:
            st, r = self.rpc(tok["access_token"], "tools/list")
        finally:
            with gateway._tunnel_lock:
                gateway._tunnels[self.tunnel.tid]["org_id"] = "orgA"
        self.assertEqual(st, 502, r)
        self.assertEqual(len(self.machine_a.calls), before, "nothing was relayed")

    def test_only_the_tunnel_owner_may_answer_a_relayed_request(self):
        rid = secrets.token_urlsafe(10)
        ev = threading.Event()
        with gateway._tunnel_lock:
            gateway._tunnel_pending[rid] = {"ev": ev, "resp": None, "tid": self.tunnel.tid}
        try:
            reg = self.gw.call("POST", "/tunnel/register", headers={"Authorization": "Bearer tokB"})[1]
            self.assertTrue(reg["tid"])
            st, _ = self.gw.call("POST", f"/tunnel/reply?rid={rid}", body={"status": 200, "body": ""},
                                 headers={"Authorization": "Bearer tokB"})
            self.assertEqual(st, 403)
            self.assertFalse(ev.is_set(), "bob's forged answer was not delivered")
        finally:
            with gateway._tunnel_lock:
                gateway._tunnel_pending.pop(rid, None)

    def test_a_replayed_code_kills_the_grant(self):
        cid, tok, code, verifier = self.connect("alice", "container:orgA")
        st, r = self.gw.call("POST", "/oauth/token", form={
            "grant_type": "authorization_code", "code": code, "client_id": cid,
            "redirect_uri": CHATGPT_CB, "code_verifier": verifier})
        self.assertEqual((st, r["error_description"]), (400, "code_replayed"))
        st, _ = self.rpc(tok["access_token"], "tools/list")
        self.assertEqual(st, 401, "the first token died with its grant")

    def test_pkce_and_client_are_checked_at_the_token_endpoint(self):
        cid = self.register()
        verifier = secrets.token_urlsafe(40)
        _, req, k = self.picker("alice", cid, verifier)
        self.gw.call("POST", "/oauth/authorize/choose", form={"req": req, "k": k, "ws": "container:orgA"}, headers=cookie("alice"))
        st, w = self.gw.call("GET", f"/oauth/authorize/wait?req={req}&k={k}")
        code = urllib.parse.parse_qs(urllib.parse.urlparse(w["redirect"]).query)["code"][0]
        st, r = self.gw.call("POST", "/oauth/token", form={"grant_type": "authorization_code", "code": code,
                                                           "client_id": cid, "redirect_uri": CHATGPT_CB,
                                                           "code_verifier": "x" * 50})
        self.assertEqual((st, r["error_description"]), (400, "pkce_failed"))

    def test_refresh_rotates_and_the_old_refresh_dies(self):
        cid, tok, _, _ = self.connect("alice", "container:orgA")
        st, t2 = self.gw.call("POST", "/oauth/token", form={"grant_type": "refresh_token",
                                                            "refresh_token": tok["refresh_token"], "client_id": cid})
        self.assertEqual(st, 200)
        st, r = self.gw.call("POST", "/oauth/token", form={"grant_type": "refresh_token",
                                                           "refresh_token": tok["refresh_token"], "client_id": cid})
        self.assertEqual((st, r["error_description"]), (400, "token_revoked"))
        self.assertEqual(self.rpc(t2["access_token"], "tools/list")[0], 200)

    def test_revoking_on_the_workspace_ends_the_grant(self):
        _, tok, _, _ = self.connect("bob", "container:orgB")
        self.assertEqual(self.rpc(tok["access_token"], "tools/list")[0], 200)
        self.ws_b.revoke_all()
        st, _ = self.rpc(tok["access_token"], "tools/list")
        self.assertEqual(st, 401, "workspace said no, refresh failed, so ChatGPT must reconnect")
        grants = self.gw.call("GET", "/api/gateway/chatgpt/grants", headers=cookie("bob"))[1]["grants"]
        self.assertTrue(any(g["status"] == "revoked" for g in grants))

    def test_the_user_can_list_and_revoke_their_connections_and_nobody_elses(self):
        mine = lambda: {g["id"] for g in self.gw.call("GET", "/api/gateway/chatgpt/grants", headers=cookie("alice"))[1]["grants"]}
        before = mine()
        _, tok, _, _ = self.connect("alice", "container:orgA")
        (gid,) = mine() - before
        st, _ = self.gw.call("POST", f"/api/gateway/chatgpt/grants/{gid}/revoke", headers=cookie("bob"))
        self.assertEqual(st, 403)
        st, _ = self.gw.call("POST", f"/api/gateway/chatgpt/grants/{gid}/revoke", headers=cookie("alice"))
        self.assertEqual(st, 200)
        self.assertEqual(self.rpc(tok["access_token"], "tools/list")[0], 401)

    def test_register_refuses_unlisted_redirects(self):
        st, r = self.gw.call("POST", "/oauth/register", body={"redirect_uris": ["https://evil.test/cb"]})
        self.assertEqual((st, r["error"]), (400, "invalid_redirect_uri"))


if __name__ == "__main__":
    unittest.main(verbosity=2)
