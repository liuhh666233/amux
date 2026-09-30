"""The ChatGPT front door: one public origin for every user's amux (AMUX-5397).

A ChatGPT directory listing names ONE MCP URL for every user, and its origin
can never change after listing. Each amux (a cloud workspace container, or a
user's own machine reached through its tunnel) already is a complete MCP
server with its own OAuth (crates/amux-server/src/api/chatgpt_app.rs). This
module puts one origin, https://cloud.amux.io, in front of all of them.

# The design, and why this one

The gateway is the OAuth authorization server ChatGPT sees, and it is also an
ordinary OAuth CLIENT of the workspace the user picks. Nothing new is needed
on the workspace side except two things it already almost had: its redirect
allow-list admits exactly this front door's callback, and its authorize
endpoint answers JSON when asked. So the workspace keeps every rule it has
(its own tokens, its own tool gates, the owner approving each connection) and
the front door adds only routing and a token of its own.

The flow, for a user who connects amux in ChatGPT:

1. ChatGPT registers here and sends the user to /oauth/authorize.
2. The user signs in with the same Clerk session the dashboard uses and picks
   one workspace from the ones they may reach: a cloud workspace they own or
   administer, or a live tunnel registered by one of their orgs.
3. The front door runs the workspace's own OAuth (register, authorize with
   PKCE) server-side. For a tunnel, the machine's owner approves the shown
   code in that amux's Settings > ChatGPT, exactly as for a direct
   connection. For a cloud workspace, whose dashboard sits behind this same
   gateway and cannot tell a relayed approval from a forged one, the owner's
   click on this consent page is the approval, and the gateway records it on
   the workspace from loopback, the one hop the workspace trusts.
4. The front door redeems the workspace code, keeps the workspace tokens,
   mints its own code for ChatGPT, and ChatGPT exchanges that for a front
   door access token.
5. POST /mcp with that token is relayed, body unchanged, to the grant's one
   workspace with the workspace token. The workspace's answer (including
   every refusal) goes back verbatim.

Rejected alternative: proxying ChatGPT's OAuth straight through to the
workspace. Registration happens before the user is known, so there is no
workspace to proxy it to, and a workspace token presented at the shared /mcp
does not say which workspace it belongs to without a lookup table, which is
this design with extra steps.

# The tenant rule

A front door grant is bound to (user, org, workspace) when it is created, and
nothing a request carries can change that binding:

- the picker only offers, and the choose step only accepts, a workspace the
  signed-in user may reach (checked again at choose time, not trusted from
  the form);
- /mcp resolves the workspace from the grant, never from the request, and a
  tunnel is only reachable while the tunnel registered under that tid still
  belongs to the grant's org (a re-registration by another org breaks the
  grant instead of redirecting it);
- the workspace verifies its own token on every call, so the loopback bypass
  of the workspace server is never what admits a request.

Every refusal prints one `[chatgpt-front] verdict=<name>` line.
"""

import base64
import hashlib
import html
import json
import secrets
import threading
import time
from urllib.parse import parse_qs, urlencode, urlparse

ACCESS_TTL = 3600
REFRESH_TTL = 30 * 86400
CODE_TTL = 120
REQUEST_TTL = 900
SCOPES = ("amux:read", "amux:write")

_lock = threading.Lock()


def verdict(name, **kv):
    """One greppable line per refusal or state change."""
    extra = " ".join(f"{k}={v}" for k, v in kv.items())
    print(f"[chatgpt-front] verdict={name} {extra}".rstrip(), flush=True)


def _rand(n=24):
    return secrets.token_hex(n)


def _h(s):
    return hashlib.sha256(s.encode()).hexdigest()


def pkce_s256_ok(verifier, challenge):
    if not (43 <= len(verifier) <= 128):
        return False
    if not all(c.isalnum() or c in "-._~" for c in verifier):
        return False
    got = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
    return secrets.compare_digest(got, challenge)


def pkce_pair():
    verifier = secrets.token_urlsafe(48)[:64]
    challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
    return verifier, challenge


def redirect_allowed(uri, extra=""):
    """ChatGPT's callbacks, loopback for local MCP clients, plus listed https
    prefixes. The same rule the workspace applies (chatgpt_app.rs)."""
    try:
        u = urlparse(uri)
    except Exception:
        return False
    if u.fragment or u.username or u.password:
        return False
    host = (u.hostname or "").lower()
    chatgpt = (u.scheme == "https" and host == "chatgpt.com" and u.port is None
               and (u.path == "/connector_platform_oauth_redirect" or u.path.startswith("/connector/oauth/")))
    loopback = u.scheme == "http" and host in ("localhost", "127.0.0.1", "::1")
    listed = any(uri.startswith(p) for p in (x.strip() for x in extra.split(","))
                 if p.startswith("https://") and len(p) > len("https://x/"))
    return chatgpt or loopback or listed


def normalize_scope(requested):
    parts = [p for p in (requested or "").split() if p]
    if not parts:
        return " ".join(SCOPES)
    if any(p not in SCOPES for p in parts):
        return None
    return " ".join(s for s in SCOPES if s in parts)


def ensure_schema(db):
    db.executescript("""
        CREATE TABLE IF NOT EXISTS front_clients (
            client_id     TEXT PRIMARY KEY,
            name          TEXT,
            redirect_uris TEXT NOT NULL,
            created_at    INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS front_requests (
            id             TEXT PRIMARY KEY,
            client_id      TEXT NOT NULL,
            redirect_uri   TEXT NOT NULL,
            code_challenge TEXT NOT NULL,
            scope          TEXT NOT NULL,
            state          TEXT,
            user_id        TEXT NOT NULL,
            secret         TEXT NOT NULL,
            status         TEXT NOT NULL,
            org_id         TEXT,
            ws_kind        TEXT,
            ws_ref         TEXT,
            ws_client_id   TEXT,
            ws_verifier    TEXT,
            ws_poll        TEXT,
            ws_user_code   TEXT,
            grant_id       TEXT,
            code_hash      TEXT,
            code_expires   INTEGER,
            code_used      INTEGER NOT NULL DEFAULT 0,
            message        TEXT,
            created_at     INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS front_grants (
            id            TEXT PRIMARY KEY,
            client_id     TEXT NOT NULL,
            user_id       TEXT NOT NULL,
            org_id        TEXT NOT NULL,
            ws_kind       TEXT NOT NULL,
            ws_ref        TEXT NOT NULL,
            ws_label      TEXT,
            scope         TEXT NOT NULL,
            ws_client_id  TEXT NOT NULL,
            ws_access     TEXT,
            ws_refresh    TEXT,
            status        TEXT NOT NULL,
            created_at    INTEGER NOT NULL,
            last_used     INTEGER,
            revoked_at    INTEGER
        );
        CREATE TABLE IF NOT EXISTS front_tokens (
            token_hash  TEXT PRIMARY KEY,
            grant_id    TEXT NOT NULL,
            client_id   TEXT NOT NULL,
            kind        TEXT NOT NULL,
            expires     INTEGER NOT NULL,
            revoked_at  INTEGER
        );
    """)
    db.commit()


class Env:
    """What the front door needs from its host. gateway.py supplies the real
    one; tests supply fakes. Every method is required.

    origin                     the one public origin, e.g. https://cloud.amux.io
    db()                       a sqlite3 connection (row_factory=Row)
    identify(handler)          signed-in user id, or None
    login(handler, return_to)  answer the request with the sign-in page
    workspaces(user_id)        [{"kind","ref","org_id","label","role"}] the user may reach
    ws_fetch(kind, ref, org_id, method, path, headers, body)
                               -> (status, headers, bytes); raises LookupError
                               when that workspace is not reachable FOR THAT ORG
    redirect_extra             extra allowed redirect prefixes (comma list)
    """


# ── helpers over the handler (BaseHTTPRequestHandler) ────────────────────────

def _send(handler, status, body, ctype="application/json", headers=None):
    if isinstance(body, (dict, list)):
        body = json.dumps(body)
    data = body.encode() if isinstance(body, str) else body
    handler.send_response(status)
    handler.send_header("Content-Type", ctype)
    handler.send_header("Content-Length", str(len(data)))
    handler.send_header("Cache-Control", "no-store")
    for k, v in (headers or {}).items():
        handler.send_header(k, v)
    handler.end_headers()
    if handler.command != "HEAD":
        handler.wfile.write(data)


def _page(handler, title, inner, status=200):
    doc = ("<!doctype html><html><head><meta charset=utf-8>"
           "<meta name=viewport content='width=device-width,initial-scale=1'>"
           f"<title>{html.escape(title)}</title><style>"
           "body{font:16px -apple-system,system-ui,sans-serif;background:#111;color:#eee;display:flex;"
           "min-height:100vh;align-items:center;justify-content:center;margin:0}main{max-width:440px;padding:24px}"
           "button{font:inherit;width:100%;min-height:44px;margin:6px 0;border-radius:8px;border:1px solid #444;"
           "background:#222;color:#eee;text-align:left;padding:10px 14px;cursor:pointer}"
           "code{font-size:28px;letter-spacing:3px;background:#222;padding:6px 12px;border-radius:6px}"
           ".dim{color:#999}</style></head>"
           f"<body><main>{inner}</main></body></html>")
    _send(handler, status, doc, "text/html; charset=utf-8", {"X-Frame-Options": "DENY"})


def _oauth_error(handler, status, error, desc, name, **kv):
    verdict(name, error=error, **kv)
    _send(handler, status, {"error": error, "error_description": desc})


def _body(handler):
    n = int(handler.headers.get("Content-Length", 0) or 0)
    return handler.rfile.read(n) if n else b""


def _form(raw):
    return {k: v[0] for k, v in parse_qs(raw.decode(errors="replace"), keep_blank_values=True).items()}


def _redirect(handler, loc):
    handler.send_response(302)
    handler.send_header("Location", loc)
    handler.send_header("Content-Length", "0")
    handler.end_headers()


def _with_params(uri, params):
    return uri + ("&" if "?" in uri else "?") + urlencode(params)


# ── metadata ─────────────────────────────────────────────────────────────────

def protected_resource(origin):
    return {"resource": origin + "/mcp", "authorization_servers": [origin],
            "scopes_supported": list(SCOPES), "bearer_methods_supported": ["header"],
            "resource_name": "amux", "resource_documentation": "https://amux.io/"}


def auth_server(origin):
    return {"issuer": origin,
            "authorization_endpoint": origin + "/oauth/authorize",
            "token_endpoint": origin + "/oauth/token",
            "registration_endpoint": origin + "/oauth/register",
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none"],
            "scopes_supported": list(SCOPES),
            "authorization_response_iss_parameter_supported": True}


def challenge_header(origin, error=None):
    s = f'Bearer resource_metadata="{origin}/.well-known/oauth-protected-resource", scope="{" ".join(SCOPES)}"'
    if error:
        s += f', error="{error}"'
    return s


# ── the workspace as an OAuth provider (the front door is its client) ────────

def workspace_callback(origin):
    return origin + "/oauth/workspace/callback"


def _ws_json(env, g, method, path, body=None, form=None, headers=None):
    """Call one workspace endpoint. g carries kind/ref/org_id. Returns (status, json)."""
    hs = {"Accept": "application/json"}
    hs.update(headers or {})
    data = None
    if form is not None:
        data = urlencode(form).encode()
        hs["Content-Type"] = "application/x-www-form-urlencoded"
    elif body is not None:
        data = json.dumps(body).encode()
        hs["Content-Type"] = "application/json"
    status, _h2, raw = env.ws_fetch(g["ws_kind"], g["ws_ref"], g["org_id"], method, path, hs, data)
    try:
        return status, json.loads(raw or b"null")
    except ValueError:
        return status, None


def _start_workspace_grant(env, req, ws):
    """Register with the workspace and open a pending request there. Returns
    (ok, message). On success the request row carries the workspace poll."""
    g = {"ws_kind": ws["kind"], "ws_ref": ws["ref"], "org_id": ws["org_id"]}
    cb = workspace_callback(env.origin)
    st, reg = _ws_json(env, g, "POST", "/oauth/register",
                       body={"client_name": "amux cloud (ChatGPT)", "redirect_uris": [cb],
                             "token_endpoint_auth_method": "none"})
    if st != 201 or not isinstance(reg, dict) or not reg.get("client_id"):
        verdict("front_ws_register_failed", status=st, kind=ws["kind"], ref=ws["ref"])
        return False, ("That amux did not accept the connection. Update it, and for a machine "
                       "turn on Settings > ChatGPT > Publish connector.")
    verifier, challenge = pkce_pair()
    q = urlencode({"response_type": "code", "client_id": reg["client_id"], "redirect_uri": cb,
                   "code_challenge": challenge, "code_challenge_method": "S256",
                   "scope": req["scope"], "state": req["id"]})
    st, pend = _ws_json(env, g, "GET", "/oauth/authorize?" + q)
    if st != 200 or not isinstance(pend, dict) or pend.get("status") != "pending":
        verdict("front_ws_authorize_failed", status=st, kind=ws["kind"], ref=ws["ref"])
        return False, "That amux is too old to connect this way. Update it and try again."
    if ws["kind"] == "container":
        # The signed-in owner's click on our consent page is the approval (see
        # the module docstring). Only an owner or admin may give it.
        if ws.get("role") not in ("owner", "admin"):
            verdict("front_container_approval_refused", role=ws.get("role"), org=ws["org_id"])
            return False, "Only an owner or admin of that workspace can connect it."
        st, _ = _ws_json(env, g, "POST", f"/api/chatgpt-app/requests/{pend['request']}/approve", body={})
        if st != 200:
            verdict("front_container_approve_failed", status=st, org=ws["org_id"])
            return False, "The workspace refused the approval."
    with _lock:
        db = env.db()
        db.execute("UPDATE front_requests SET status='waiting', org_id=?, ws_kind=?, ws_ref=?, ws_client_id=?, "
                   "ws_verifier=?, ws_poll=?, ws_user_code=? WHERE id=?",
                   (ws["org_id"], ws["kind"], ws["ref"], reg["client_id"], verifier, pend["poll"],
                    pend.get("user_code", ""), req["id"]))
        db.commit()
    verdict("front_ws_request_open", kind=ws["kind"], ref=ws["ref"], request=req["id"])
    return True, ""


def _finish_if_approved(env, req):
    """Poll the workspace; on approval redeem its code, store the grant and
    mint our own code. Returns a dict for the waiting page."""
    g = {"ws_kind": req["ws_kind"], "ws_ref": req["ws_ref"], "org_id": req["org_id"]}
    st, p = _ws_json(env, g, "GET", "/" + req["ws_poll"])
    if st != 200 or not isinstance(p, dict):
        return {"status": "pending", "note": "workspace unreachable, retrying"}
    s = p.get("status")
    if s == "pending":
        return {"status": "pending"}
    if s != "approved":
        with _lock:
            db = env.db()
            db.execute("UPDATE front_requests SET status=?, message=? WHERE id=?",
                       ("denied" if s == "denied" else "expired", p.get("message", ""), req["id"]))
            db.commit()
        verdict("front_ws_not_approved", status=s, request=req["id"])
        return {"status": s, "message": p.get("message") or s}
    q = parse_qs(urlparse(p.get("redirect", "")).query)
    code = (q.get("code") or [""])[0]
    if (q.get("state") or [""])[0] != req["id"] or not code:
        verdict("front_ws_redirect_mismatch", request=req["id"])
        return {"status": "error", "message": "The workspace answered for a different request."}
    st, tok = _ws_json(env, g, "POST", "/oauth/token", form={
        "grant_type": "authorization_code", "code": code, "client_id": req["ws_client_id"],
        "redirect_uri": workspace_callback(env.origin), "code_verifier": req["ws_verifier"]})
    if st != 200 or not isinstance(tok, dict) or not tok.get("access_token"):
        verdict("front_ws_token_failed", status=st, request=req["id"])
        return {"status": "error", "message": "The workspace would not issue a token."}
    now = int(time.time())
    grant = "fg_" + _rand(12)
    code_ours = "fc_" + _rand(24)
    label = next((w["label"] for w in env.workspaces(req["user_id"])
                  if w["kind"] == req["ws_kind"] and w["ref"] == req["ws_ref"]), req["ws_ref"])
    with _lock:
        db = env.db()
        db.execute("INSERT INTO front_grants (id, client_id, user_id, org_id, ws_kind, ws_ref, ws_label, scope, "
                   "ws_client_id, ws_access, ws_refresh, status, created_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
                   (grant, req["client_id"], req["user_id"], req["org_id"], req["ws_kind"], req["ws_ref"], label,
                    req["scope"], req["ws_client_id"], tok["access_token"], tok.get("refresh_token", ""),
                    "active", now))
        redirect = _with_params(req["redirect_uri"], {"code": code_ours, "state": req["state"] or "",
                                                      "iss": env.origin})
        db.execute("UPDATE front_requests SET status='approved', grant_id=?, code_hash=?, code_expires=?, message=? "
                   "WHERE id=?", (grant, _h(code_ours), now + CODE_TTL, redirect, req["id"]))
        db.commit()
    verdict("front_grant_created", grant=grant, kind=req["ws_kind"], ref=req["ws_ref"], org=req["org_id"])
    return {"status": "approved", "redirect": redirect}


# ── route handlers ───────────────────────────────────────────────────────────

FRONT_PATHS = {
    "/mcp", "/oauth/register", "/oauth/authorize", "/oauth/authorize/choose", "/oauth/authorize/wait",
    "/oauth/token", "/oauth/workspace/callback",
    "/.well-known/oauth-protected-resource", "/.well-known/oauth-protected-resource/mcp",
    "/.well-known/oauth-authorization-server", "/.well-known/openid-configuration",
}


def handle(handler, path, qs, env):
    """Serve a front door path. Returns True when handled."""
    if path not in FRONT_PATHS:
        return False
    m = handler.command
    if path.startswith("/.well-known/oauth-protected-resource") and m == "GET":
        _send(handler, 200, protected_resource(env.origin))
    elif path.startswith("/.well-known/") and m == "GET":
        _send(handler, 200, auth_server(env.origin))
    elif path == "/oauth/register" and m == "POST":
        _register(handler, env)
    elif path == "/oauth/authorize" and m == "GET":
        _authorize(handler, qs, env)
    elif path == "/oauth/authorize/choose" and m == "POST":
        _choose(handler, env)
    elif path == "/oauth/authorize/wait" and m == "GET":
        _wait(handler, qs, env)
    elif path == "/oauth/token" and m == "POST":
        _token(handler, env)
    elif path == "/oauth/workspace/callback" and m == "GET":
        _page(handler, "amux", "<p>You can close this window.</p>")
    elif path == "/mcp" and m == "POST":
        _mcp(handler, env)
    elif path == "/mcp":
        _send(handler, 405, "This MCP endpoint is stateless: POST JSON-RPC only.", "text/plain", {"Allow": "POST"})
    else:
        _send(handler, 405, {"error": "method not allowed"})
    return True


def _register(handler, env):
    try:
        v = json.loads(_body(handler) or b"{}")
    except ValueError:
        v = {}
    uris = [u for u in (v.get("redirect_uris") or []) if isinstance(u, str)]
    if not uris or len(uris) > 10:
        return _oauth_error(handler, 400, "invalid_redirect_uri", "redirect_uris must list 1 to 10 URIs",
                            "front_register_no_redirects")
    bad = next((u for u in uris if not redirect_allowed(u, env.redirect_extra)), None)
    if bad:
        return _oauth_error(handler, 400, "invalid_redirect_uri", f"{bad} is not an allowed redirect",
                            "front_register_redirect_refused")
    if (v.get("token_endpoint_auth_method") or "none") != "none":
        return _oauth_error(handler, 400, "invalid_client_metadata", "only public clients with PKCE",
                            "front_register_auth_method_refused")
    cid = "fcl_" + _rand(12)
    name = str(v.get("client_name") or "")[:80]
    now = int(time.time())
    with _lock:
        db = env.db()
        db.execute("INSERT INTO front_clients (client_id, name, redirect_uris, created_at) VALUES (?,?,?,?)",
                   (cid, name, json.dumps(uris), now))
        db.commit()
    verdict("front_client_registered", client=cid)
    _send(handler, 201, {"client_id": cid, "client_id_issued_at": now, "client_name": name,
                         "redirect_uris": uris, "token_endpoint_auth_method": "none",
                         "grant_types": ["authorization_code", "refresh_token"], "response_types": ["code"]})


def _client(env, cid):
    r = env.db().execute("SELECT name, redirect_uris FROM front_clients WHERE client_id=?", (cid,)).fetchone()
    return (r["name"], json.loads(r["redirect_uris"])) if r else None


def _authorize(handler, qs, env):
    kv = {k: v[0] for k, v in parse_qs(qs, keep_blank_values=True).items()}
    cid, ruri = kv.get("client_id", ""), kv.get("redirect_uri", "")
    client = _client(env, cid)
    if not client:
        verdict("front_authorize_unknown_client", client=cid)
        return _page(handler, "amux", "<h2>Unknown client</h2><p class=dim>Start the connection again from ChatGPT.</p>", 400)
    name, uris = client
    if ruri not in uris or not redirect_allowed(ruri, env.redirect_extra):
        verdict("front_authorize_redirect_mismatch", client=cid)
        return _page(handler, "amux", "<h2>Redirect not allowed</h2>", 400)

    def bounce(err, desc, name_):
        verdict(name_, error=err)
        return _redirect(handler, _with_params(ruri, {"error": err, "error_description": desc,
                                                      "state": kv.get("state", ""), "iss": env.origin}))
    if kv.get("response_type") != "code":
        return bounce("unsupported_response_type", "only response_type=code", "front_authorize_response_type")
    ch = kv.get("code_challenge", "")
    if kv.get("code_challenge_method") != "S256" or len(ch) < 43:
        return bounce("invalid_request", "PKCE S256 is required", "front_authorize_pkce_missing")
    scope = normalize_scope(kv.get("scope", ""))
    if scope is None:
        return bounce("invalid_scope", "supported scopes: amux:read amux:write", "front_authorize_bad_scope")
    res = kv.get("resource", "").rstrip("/")
    if res and res != env.origin + "/mcp":
        return bounce("invalid_target", "resource must be this server's /mcp URL", "front_authorize_resource_mismatch")
    user = env.identify(handler)
    if not user:
        return env.login(handler, "/oauth/authorize?" + qs)
    rid, secret = "fr_" + _rand(12), _rand(24)
    with _lock:
        db = env.db()
        db.execute("INSERT INTO front_requests (id, client_id, redirect_uri, code_challenge, scope, state, user_id, "
                   "secret, status, created_at) VALUES (?,?,?,?,?,?,?,?,?,?)",
                   (rid, cid, ruri, ch, scope, kv.get("state", ""), user, secret, "choosing", int(time.time())))
        db.commit()
    wss = env.workspaces(user)
    if not wss:
        verdict("front_authorize_no_workspace", user=user)
        return _page(handler, "Connect amux",
                     "<h2>No amux to connect</h2><p class=dim>Open amux on your machine and turn on "
                     "Settings &gt; ChatGPT &gt; Publish connector, then start again from ChatGPT.</p>")
    esc = html.escape
    buttons = "".join(
        f"<button name=ws value='{esc(w['kind'])}:{esc(w['ref'])}'>{esc(w['label'])}"
        f"<br><span class=dim>{'cloud workspace' if w['kind'] == 'container' else 'your machine, through its tunnel'}"
        f"</span></button>" for w in wss)
    _page(handler, "Connect amux",
          f"<h2>Connect {esc(name or 'ChatGPT')} to amux</h2>"
          f"<p class=dim>It will be able to: {esc(scope.replace('amux:', '').replace(' ', ' and '))} "
          f"workers and board cards.</p><p class=dim>Continue only if you started this from your own "
          f"ChatGPT just now. If someone sent you this link, close this page.</p><p>Pick the amux to connect:</p>"
          f"<form method=post action='/oauth/authorize/choose'>"
          f"<input type=hidden name=req value='{rid}'><input type=hidden name=k value='{secret}'>{buttons}</form>")


def _load_req(env, rid, secret, user):
    r = env.db().execute("SELECT * FROM front_requests WHERE id=?", (rid,)).fetchone()
    if not r or not secrets.compare_digest(r["secret"], secret or ""):
        return None, "unknown_request"
    if user is not None and r["user_id"] != user:
        return None, "different_user"
    if r["created_at"] + REQUEST_TTL < time.time():
        return None, "request_expired"
    return dict(r), ""


def _choose(handler, env):
    f = _form(_body(handler))
    user = env.identify(handler)
    if not user:
        verdict("front_choose_signed_out")
        return _page(handler, "amux", "<h2>Signed out</h2><p class=dim>Start again from ChatGPT.</p>", 401)
    req, why = _load_req(env, f.get("req", ""), f.get("k", ""), user)
    if not req or req["status"] != "choosing":
        verdict("front_choose_refused", why=why or "not_choosing")
        return _page(handler, "amux", "<h2>This request is no longer valid</h2>", 400)
    kind, _, ref = f.get("ws", "").partition(":")
    # The tenant rule: re-check the pick against what THIS user may reach now.
    ws = next((w for w in env.workspaces(user) if w["kind"] == kind and w["ref"] == ref), None)
    if not ws:
        verdict("front_choose_workspace_not_yours", user=user, kind=kind, ref=ref)
        return _page(handler, "amux", "<h2>You cannot connect that workspace</h2>", 403)
    try:
        ok, msg = _start_workspace_grant(env, req, ws)
    except LookupError as e:
        verdict("front_choose_workspace_unreachable", kind=kind, ref=ref, why=str(e))
        ok, msg = False, "That amux is not reachable right now. Is it running, with Publish connector on?"
    if not ok:
        return _page(handler, "amux", f"<h2>Could not connect</h2><p class=dim>{html.escape(msg)}</p>", 502)
    req = dict(env.db().execute("SELECT * FROM front_requests WHERE id=?", (req["id"],)).fetchone())
    if ws["kind"] == "container":
        lead = "<h2>Connecting</h2><p class=dim>Finishing the connection to your cloud workspace.</p>"
    else:
        lead = ("<h2>Approve on your machine</h2><p>Open amux on that machine, go to <b>Settings &gt; ChatGPT</b>, "
                f"and approve this code:</p><p><code>{html.escape(req['ws_user_code'] or '')}</code></p>")
    _page(handler, "Approve amux connection",
          lead + "<p class=dim id=s>Waiting. This page continues by itself.</p>"
          "<script>(function(){var u='/oauth/authorize/wait?req=" + req["id"] + "&k=" + req["secret"] + "';"
          "function t(){fetch(u,{cache:'no-store'}).then(function(r){return r.json()}).then(function(d){"
          "if(d.redirect){location.replace(d.redirect);return}"
          "if(d.status!=='pending'){document.getElementById('s').textContent=d.message||d.status;return}"
          "setTimeout(t,2000)}).catch(function(){setTimeout(t,4000)})}t()})()</script>")


_finish_lock = threading.Lock()


def _wait(handler, qs, env):
    kv = {k: v[0] for k, v in parse_qs(qs).items()}
    # Serialized, and the request re-read inside the lock: the workspace mints
    # its code once, on the first poll after approval. Two overlapping polls
    # (a slow network, a second tab) would otherwise both ask, and the loser
    # would see "expired" and overwrite the winner's result.
    with _finish_lock:
        req, why = _load_req(env, kv.get("req", ""), kv.get("k", ""), None)
        if not req:
            verdict("front_wait_refused", why=why)
            return _send(handler, 404, {"status": "unknown", "message": "Unknown or expired request."})
        if req["status"] == "approved":
            # Never mint twice. A lost response is retried by the same secret
            # holder, so hand back the same one-time redirect while it is live.
            if not req["code_used"] and (req["code_expires"] or 0) >= time.time() and req["message"]:
                return _send(handler, 200, {"status": "approved", "redirect": req["message"]})
            return _send(handler, 200, {"status": "done", "message": "Already connected. Return to ChatGPT."})
        if req["status"] != "waiting":
            return _send(handler, 200, {"status": req["status"], "message": req["message"] or req["status"]})
        try:
            out = _finish_if_approved(env, req)
        except LookupError as e:
            verdict("front_wait_workspace_unreachable", why=str(e))
            out = {"status": "pending", "note": "workspace unreachable, retrying"}
    _send(handler, 200, out)


def _issue(env, grant, cid, scope):
    now = int(time.time())
    access, refresh = "fat_" + _rand(24), "frt_" + _rand(24)
    with _lock:
        db = env.db()
        db.execute("INSERT INTO front_tokens VALUES (?,?,?,?,?,NULL)", (_h(access), grant, cid, "access", now + ACCESS_TTL))
        db.execute("INSERT INTO front_tokens VALUES (?,?,?,?,?,NULL)", (_h(refresh), grant, cid, "refresh", now + REFRESH_TTL))
        db.commit()
    return {"access_token": access, "token_type": "Bearer", "expires_in": ACCESS_TTL,
            "refresh_token": refresh, "scope": scope}


def revoke_grant(env, grant, why):
    now = int(time.time())
    with _lock:
        db = env.db()
        db.execute("UPDATE front_grants SET status='revoked', revoked_at=? WHERE id=? AND status!='revoked'", (now, grant))
        db.execute("UPDATE front_tokens SET revoked_at=? WHERE grant_id=? AND revoked_at IS NULL", (now, grant))
        db.commit()
    verdict("front_grant_revoked", grant=grant, why=why)


def _token(handler, env):
    f = _form(_body(handler))
    cid, gt = f.get("client_id", ""), f.get("grant_type", "")
    now = int(time.time())
    if gt == "authorization_code":
        r = env.db().execute("SELECT * FROM front_requests WHERE code_hash=?", (_h(f.get("code", "")),)).fetchone()
        if not r:
            return _oauth_error(handler, 400, "invalid_grant", "unknown_code", "front_token_refused", why="unknown_code")
        if r["code_used"]:
            revoke_grant(env, r["grant_id"], "code_replayed")
            return _oauth_error(handler, 400, "invalid_grant", "code_replayed", "front_token_refused", why="code_replayed")
        with _lock:
            db = env.db()
            db.execute("UPDATE front_requests SET code_used=1, message=NULL WHERE id=?", (r["id"],))
            db.commit()
        checks = [
            (r["code_expires"] < now, "code_expired"),
            (r["client_id"] != cid, "client_mismatch"),
            (r["redirect_uri"] != f.get("redirect_uri", ""), "redirect_uri_mismatch"),
            (f.get("resource") and f["resource"].rstrip("/") != env.origin + "/mcp", "resource_mismatch"),
            (not pkce_s256_ok(f.get("code_verifier", ""), r["code_challenge"]), "pkce_failed"),
        ]
        bad = next((w for c, w in checks if c), None)
        if bad:
            return _oauth_error(handler, 400, "invalid_grant", bad, "front_token_refused", why=bad)
        return _send(handler, 200, _issue(env, r["grant_id"], cid, r["scope"]))
    if gt == "refresh_token":
        h = _h(f.get("refresh_token", ""))
        t = env.db().execute("SELECT * FROM front_tokens WHERE token_hash=? AND kind='refresh'", (h,)).fetchone()
        g = t and env.db().execute("SELECT * FROM front_grants WHERE id=?", (t["grant_id"],)).fetchone()
        why = ("unknown_refresh" if not t else "token_revoked" if t["revoked_at"] else
               "token_expired" if t["expires"] < now else "client_mismatch" if t["client_id"] != cid else
               "grant_inactive" if not g or g["status"] != "active" else "")
        if why:
            return _oauth_error(handler, 400, "invalid_grant", why, "front_token_refused", why=why)
        with _lock:
            db = env.db()
            db.execute("UPDATE front_tokens SET revoked_at=? WHERE token_hash=?", (now, h))
            db.commit()
        return _send(handler, 200, _issue(env, g["id"], cid, g["scope"]))
    return _oauth_error(handler, 400, "unsupported_grant_type", f"grant_type {gt!r}", "front_token_bad_grant_type")


def check_access(env, token):
    """(grant row, "") or (None, why). The binding a token can never change."""
    t = env.db().execute("SELECT * FROM front_tokens WHERE token_hash=? AND kind='access'", (_h(token),)).fetchone()
    if not t:
        return None, "unknown_token"
    if t["revoked_at"]:
        return None, "token_revoked"
    if t["expires"] < time.time():
        return None, "token_expired"
    g = env.db().execute("SELECT * FROM front_grants WHERE id=?", (t["grant_id"],)).fetchone()
    if not g or g["status"] != "active":
        return None, "grant_inactive"
    return dict(g), ""


def _unauthorized(handler, env, why):
    verdict("front_mcp_unauthorized", why=why)
    _send(handler, 401, {"error": "unauthorized", "why": why},
          headers={"WWW-Authenticate": challenge_header(env.origin, None if why == "missing_token" else "invalid_token")})


def _refresh_ws(env, g):
    st, tok = _ws_json(env, g, "POST", "/oauth/token", form={
        "grant_type": "refresh_token", "refresh_token": g["ws_refresh"] or "", "client_id": g["ws_client_id"]})
    if st != 200 or not isinstance(tok, dict) or not tok.get("access_token"):
        return False
    with _lock:
        db = env.db()
        db.execute("UPDATE front_grants SET ws_access=?, ws_refresh=? WHERE id=?",
                   (tok["access_token"], tok.get("refresh_token", g["ws_refresh"]), g["id"]))
        db.commit()
    g["ws_access"], g["ws_refresh"] = tok["access_token"], tok.get("refresh_token", g["ws_refresh"])
    return True


def _mcp(handler, env):
    auth = handler.headers.get("Authorization", "")
    tok = auth[7:].strip() if auth.startswith("Bearer ") else ""
    body = _body(handler)
    if not tok:
        return _unauthorized(handler, env, "missing_token")
    g, why = check_access(env, tok)
    if not g:
        return _unauthorized(handler, env, why)

    def relay():
        hs = {"Content-Type": "application/json", "Accept": "application/json, text/event-stream",
              "Authorization": "Bearer " + (g["ws_access"] or "")}
        pv = handler.headers.get("MCP-Protocol-Version")
        if pv:
            hs["MCP-Protocol-Version"] = pv
        return env.ws_fetch(g["ws_kind"], g["ws_ref"], g["org_id"], "POST", "/mcp", hs, body)

    try:
        st, hs, raw = relay()
        if st == 401 and _refresh_ws(env, g):
            st, hs, raw = relay()
    except LookupError as e:
        # The workspace is gone or its tunnel now belongs to someone else. Never
        # re-route: tell ChatGPT the tool is unavailable and leave the grant alone
        # (a restarted tunnel under the same org comes back by itself).
        verdict("front_mcp_workspace_unreachable", grant=g["id"], kind=g["ws_kind"], why=str(e))
        return _send(handler, 502, {"error": "workspace_unreachable", "why": str(e)})
    if st == 401:
        # The owner revoked us on the workspace (or its tokens are gone): this
        # grant is dead, and ChatGPT must reconnect.
        revoke_grant(env, g["id"], "workspace_refused_token")
        return _unauthorized(handler, env, "workspace_revoked")
    with _lock:
        db = env.db()
        db.execute("UPDATE front_grants SET last_used=? WHERE id=?", (int(time.time()), g["id"]))
        db.commit()
    ctype = next((v for k, v in (hs or {}).items() if k.lower() == "content-type"), "application/json")
    _send(handler, st, raw or b"", ctype)


# ── owner surface (cookie-authed, called from gateway.py) ────────────────────

def list_grants(env, user_id):
    rows = env.db().execute(
        "SELECT id, org_id, ws_kind, ws_ref, ws_label, scope, status, created_at, last_used, revoked_at "
        "FROM front_grants WHERE user_id=? ORDER BY created_at DESC", (user_id,)).fetchall()
    return [dict(r) for r in rows]


def revoke_for_user(env, user_id, grant_id, is_org_owner):
    g = env.db().execute("SELECT * FROM front_grants WHERE id=?", (grant_id,)).fetchone()
    if not g:
        return 404
    if g["user_id"] != user_id and not is_org_owner(g["org_id"]):
        verdict("front_revoke_refused", grant=grant_id, user=user_id)
        return 403
    revoke_grant(env, grant_id, "owner_revoked")
    return 200
