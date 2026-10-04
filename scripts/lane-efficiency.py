#!/usr/bin/env python3
"""lane-efficiency: what each lane spent against what it finished.

For every lane matching --prefix (plus --orchestrator) over the last --hours:
  cost      token_ledger cost_usd (list-price tokens, not the plan's bill)
  verified  cards on the lane's board that entered `verified` in the window
  pushes    land.log lines where the lane landed (alone or batched)
  done      cards sitting at `done` now (finished, not yet confirmed)
  $/verified  cost / verified ("-" when nothing verified)

Written 2026-10-04 for Ethan ("is there anything on the harness level we can
do to make it more productive/efficient?"): the gs12 lanes spent about 15,900
dollars of list-price tokens in 24 hours for 22 verified plan items, and no
view put a lane's spend beside its output.

Population and window are printed with the table, and a lane with no
token_ledger rows says so rather than reading as free.
"""
import argparse, json, os, re, sqlite3, ssl, subprocess, sys, time, urllib.request


def amux_url():
    try:
        return subprocess.run(["amux", "url"], capture_output=True, text=True, timeout=10).stdout.strip()
    except Exception:
        return os.environ.get("AMUX_URL", "")


def board(url):
    ctx = ssl.create_default_context(); ctx.check_hostname = False; ctx.verify_mode = ssl.CERT_NONE
    with urllib.request.urlopen(f"{url}/api/board?all=1&slim=0", context=ctx, timeout=60) as r:
        return json.load(r)


def num(v):
    try:
        return float(v or 0)
    except (TypeError, ValueError):
        return 0.0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--prefix", default="gs12-")
    ap.add_argument("--orchestrator", default="mixpeek-override")
    ap.add_argument("--hours", type=float, default=24)
    ap.add_argument("--db", default=os.path.expanduser("~/.amux/amux.db"))
    ap.add_argument("--land-log", default=os.path.expanduser("~/.amux/logs/land.log"))
    ap.add_argument("--board-file", help="board JSON instead of the server (tests)")
    ap.add_argument("--now", type=float, help="epoch to measure at (tests)")
    a = ap.parse_args()
    now = a.now or time.time()
    since = now - a.hours * 3600

    def ours(s):
        return bool(s) and (s.startswith(a.prefix) or s == a.orchestrator)

    cost = {}
    db = sqlite3.connect(f"file:{a.db}?mode=ro", uri=True)
    for s, c in db.execute("SELECT session, SUM(cost_usd) FROM token_ledger WHERE ts > ? GROUP BY session", (since,)):
        if ours(s):
            cost[s] = c or 0.0

    cards = json.load(open(a.board_file)) if a.board_file else board(amux_url())
    verified, done = {}, {}
    for c in cards:
        s = c.get("session") or ""
        if not ours(s) or c.get("archived"):
            continue
        if c.get("status") == "verified" and num(c.get("entered_state_at")) >= since:
            verified[s] = verified.get(s, 0) + 1
        if c.get("status") == "done":
            done[s] = done.get(s, 0) + 1

    pushes = {}
    since_iso = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(since))
    rx = re.compile(r"^(\S+) (\S+) (landed [0-9a-f]+ on |landed in a batch)")
    if os.path.exists(a.land_log):
        for line in open(a.land_log, errors="ignore"):
            m = rx.match(line)
            if m and m.group(1) >= since_iso and ours(m.group(2)):
                pushes[m.group(2)] = pushes.get(m.group(2), 0) + 1

    lanes = sorted(set(cost) | set(verified) | set(done) | set(pushes))
    rows = []
    for s in lanes:
        c, v = cost.get(s), verified.get(s, 0)
        rows.append((s, c, v, pushes.get(s, 0), done.get(s, 0), (c / v) if (c is not None and v) else None))
    rows.sort(key=lambda r: (r[5] is None, -(r[5] or 0)))
    print(f"lane efficiency, last {a.hours:g} h (to {time.strftime('%Y-%m-%d %H:%MZ', time.gmtime(now))}); "
          f"lanes {a.prefix}* + {a.orchestrator}; cost is list-price tokens from token_ledger, not the bill")
    print(f"{'lane':<20}{'cost $':>9}{'verified':>9}{'pushes':>8}{'done':>6}{'$/verified':>12}")
    for s, c, v, p, d, per in rows:
        cs = f"{c:,.0f}" if c is not None else "no rows"
        print(f"{s:<20}{cs:>9}{v:>9}{p:>8}{d:>6}{(f'{per:,.0f}' if per is not None else '-'):>12}")
    tc = sum(x for x in cost.values()); tv = sum(verified.values())
    print(f"{'total':<20}{tc:>9,.0f}{tv:>9}{sum(pushes.values()):>8}{sum(done.values()):>6}"
          f"{(f'{tc / tv:,.0f}' if tv else '-'):>12}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
