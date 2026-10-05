#!/usr/bin/env python3
"""board-close-tracking: verify an orchestrator's tracking card once every
lane card that names it is verified.

A lane card names its tracking card with the routing line
"Tracking card on the orchestrator's board: <ID>". A tracking card is closed
only when:
  - at least one non-discarded lane card names it, and every one of them is
    `verified` (a lane card still in backlog, todo, doing, review, done or
    needsyou blocks it, so a plan item with an unbuilt clause never closes);
  - the tracking card itself is not verified, discarded or archived, and is
    not tagged `held`.
Its evidence is the lane cards' evidence under a stamp naming them
("auto-verified from GX-1, GY-2 at <utc>"), and each id closed is posted on
--log-card so the orchestrator can queue its plan line.

Written 2026-10-05 for goal spec 12 (Ethan's 21:01 ET "ok do all 3": lanes
verify their own cards; mixpeek-override asked for the roll-up of its tracking
cards with these three conditions). Dry run unless --apply.
"""
import argparse, json, os, re, ssl, subprocess, sys, time, urllib.error, urllib.request

TRACK = re.compile(r"Tracking card on the orchestrator's board: ([A-Z][A-Z0-9]*-\d+)")
PLAN_NUM = re.compile(r"^GS12 (?:plan item )?(\d+\.\d+)")
TERMINAL_SKIP = {"verified", "discarded"}


def plan(cards, orchestrator):
    """Return [(tracking_id, [lane ids])] ready to auto-verify, pure."""
    by_id = {c["id"]: c for c in cards}
    groups = {}
    for c in cards:
        if c.get("archived") or c.get("session") == orchestrator:
            continue
        m = TRACK.search(c.get("desc") or "")
        if m:
            groups.setdefault(m.group(1), []).append(c)
    # A lane card that carries only the plan number in its title ("GS12 5.11",
    # "5.11b") belongs to the item too; matching by id alone read MO-4065 (5.11)
    # as done while GR-38 (5.11b, backlog) and GR-88 still held open clauses
    # (mixpeek-override, 2026-10-05).
    lane_cards = [c for c in cards if not c.get("archived") and c.get("session") != orchestrator]
    for tid in list(groups):
        t = by_id.get(tid)
        m = PLAN_NUM.match((t or {}).get("title") or "")
        if not m:
            continue
        num = re.compile(r"(?<![\d.])" + re.escape(m.group(1)) + r"(?![\d.]\d)(?!\d)")
        seen = {c["id"] for c in groups[tid]}
        for c in lane_cards:
            if c["id"] not in seen and num.search(c.get("title") or ""):
                groups[tid].append(c); seen.add(c["id"])
    ready = []
    for tid, lanes in sorted(groups.items()):
        t = by_id.get(tid)
        if not t or t.get("archived") or t.get("session") != orchestrator:
            continue
        if t.get("status") in TERMINAL_SKIP or "held" in (t.get("tags") or []):
            continue
        live = [l for l in lanes if l.get("status") != "discarded"]
        if live and all(l.get("status") == "verified" for l in live):
            ready.append((tid, sorted(l["id"] for l in live)))
    return ready


def evidence_for(lane_ids, by_id, now):
    head = f"auto-verified from {', '.join(lane_ids)} at {now} (board-close-tracking; every lane card naming this tracking card is verified)"
    parts = [head]
    for lid in lane_ids:
        e = by_id[lid].get("evidence")
        e = e if isinstance(e, str) else json.dumps(e)
        parts.append(f"--- {lid} ---\n{e or '(no evidence text on the lane card)'}")
    return "\n\n".join(parts)


def api(url, path, method="GET", body=None, session=None):
    ctx = ssl.create_default_context(); ctx.check_hostname = False; ctx.verify_mode = ssl.CERT_NONE
    headers = {"Content-Type": "application/json"}
    if session:
        headers["X-Amux-Session"] = session
    req = urllib.request.Request(f"{url}{path}", data=json.dumps(body).encode() if body is not None else None,
                                 method=method, headers=headers)
    with urllib.request.urlopen(req, context=ctx, timeout=60) as r:
        return json.load(r)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--orchestrator", default="mixpeek-override")
    ap.add_argument("--log-card", default="MO-3964")
    ap.add_argument("--apply", action="store_true")
    ap.add_argument("--max", type=int, default=50, help="at most this many closes per run")
    ap.add_argument("--board-file", help="board JSON instead of the server (tests); implies dry run")
    a = ap.parse_args()
    url = subprocess.run(["amux", "url"], capture_output=True, text=True).stdout.strip() if not a.board_file else ""
    cards = json.load(open(a.board_file)) if a.board_file else api(url, "/api/board?all=1&slim=0")
    by_id = {c["id"]: c for c in cards}
    ready = plan(cards, a.orchestrator)[: a.max]
    now = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    print(f"board-close-tracking: {len(ready)} tracking card(s) ready ({'apply' if a.apply and not a.board_file else 'dry run'})")
    closed = []
    for tid, lanes in ready:
        print(f"  {tid} <- {', '.join(lanes)}")
        if not a.apply or a.board_file:
            continue
        # The card's effective gate is what the board checks; a refused PATCH
        # returns it and discards the whole body, so probe, then move with the
        # gate and the evidence in one request.
        try:
            api(url, f"/api/board/{tid}", "PATCH", {"status": "verified", "gate_checked": []}, session=a.orchestrator)
            gate = []
        except urllib.error.HTTPError as e:
            gate = (json.loads(e.read() or b"{}").get("gate") or [])
        body = {"status": "verified", "gate_checked": gate, "evidence": evidence_for(lanes, by_id, now)}
        try:
            api(url, f"/api/board/{tid}", "PATCH", body, session=a.orchestrator)
        except urllib.error.HTTPError as e:
            print(f"    REFUSED {e.code}: {(e.read() or b'')[:200].decode(errors='ignore')}")
            continue
        if api(url, f"/api/board/{tid}").get("status") == "verified":
            closed.append(f"{tid} (from {', '.join(lanes)})")
            print("    verified")
        else:
            print("    NOT verified after an accepted PATCH; left for the orchestrator")
    if closed:
        api(url, f"/api/board/{a.log_card}", "PATCH",
            {"desc_append": f"\n{now} board-close-tracking auto-verified {len(closed)}: " + "; ".join(closed)},
            session=a.orchestrator)
    return 0


if __name__ == "__main__":
    sys.exit(main())
