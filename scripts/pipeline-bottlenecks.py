#!/usr/bin/env python3
"""pipeline-bottlenecks: find the shared-pipeline bottleneck before a human does.

Ethan, 2026-10-01 14:16 ET: "ensure the harness proactively identifies these
bottlenecks" (AH-291). The first case is the one that held goal spec 12 for a
day: the `amux land` queue on Mixpeek main and the pre-push gate inside it
(25 deep; nothing landed for two hours after 340d477914d; median hold 23.7 min;
a stale GH_TOKEN made every gate run its slow legs in full).

Measures, per repository with an `amux land` queue (all from files on disk, no
model call):
  depth          tickets waiting
  oldest_wait    minutes the oldest ticket has waited
  holder_attempt minutes the current holder's attempt has run
  median_hold    median minutes from "acquired" to an outcome, last 6 h
  landed_6h      landings in the last 6 h
  stale_code     waiters whose land process started before the installed CLI
                 changed, so they run code without the latest queue fixes
  drain_h        hours to drain the queue at the measured rate

A repository is a BOTTLENECK when any threshold trips (all overridable):
  depth >= 8, oldest_wait >= 120, holder_attempt >= 45,
  or median_hold >= 20 with depth >= 4.

On a bottleneck it:
  1. logs one JSON verdict line to ~/.amux/logs/bottlenecks.jsonl
     (verdict=pipeline_bottleneck, measured, n_considered), and prints it;
  2. routes ONE message naming the bottleneck, its numbers and the levers to
     --route (e.g. the orchestrator whose lanes are queued), at most once per
     --cooldown-min per repository unless the drain estimate is 50% worse;
  3. never kills or reorders anything: recovery that is safe to automate
     already lives in `amux land` (stale-lock takeover, dead-ticket pruning,
     aging); this names what is left for a lane to act on.
With no queue anywhere it prints verdict=no_queues, measured=true, so a quiet
run is distinguishable from a probe that never ran.
"""
import argparse, glob, json, os, re, statistics, subprocess, sys, time

HOME = os.path.expanduser("~")
LOCKS = os.path.join(HOME, ".amux", "locks")
LAND_LOG = os.path.join(HOME, ".amux", "logs", "land.log")
OUT = os.path.join(HOME, ".amux", "logs", "bottlenecks.jsonl")
STATE = os.path.join(HOME, ".amux", "bottlenecks-state.json")
TS = re.compile(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ) (\S+) (.*?)(?: \[([0-9a-f]{16})\])?$")


def epoch(ts):
    return time.mktime(time.strptime(ts, "%Y-%m-%dT%H:%M:%SZ")) - time.timezone


def ps_start(pid):
    """Process start time (epoch) or None."""
    try:
        out = subprocess.run(["ps", "-o", "lstart=", "-p", str(pid)], capture_output=True, text=True, timeout=5).stdout.strip()
        return time.mktime(time.strptime(out, "%a %b %d %H:%M:%S %Y")) if out else None
    except Exception:
        return None


def holds_by_repo(since):
    """{repo_key: [minutes per completed hold]}, {repo_key: landings} since `since`."""
    holds, landed, open_ = {}, {}, {}
    try:
        lines = open(LAND_LOG, errors="replace").read().splitlines()
    except OSError:
        return holds, landed
    for line in lines:
        m = TS.match(line)
        if not m or not m.group(4):
            continue
        t = epoch(m.group(1))
        if t < since:
            continue
        who, msg, key = m.group(2), m.group(3), m.group(4)
        if msg.startswith("acquired"):
            open_[(key, who)] = t
        elif msg.startswith(("landed ", "push failed", "batch with", "rebase onto", "gave up")):
            if msg.startswith("landed ") and "landed in a batch" not in msg:
                landed[key] = landed.get(key, 0) + 1
            st = open_.pop((key, who), None)
            if st is not None:
                holds.setdefault(key, []).append((t - st) / 60)
    return holds, landed


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--route", default="", help="lane to message on a bottleneck (empty: log only)")
    ap.add_argument("--depth", type=int, default=8)
    ap.add_argument("--oldest-wait", type=int, default=120)
    ap.add_argument("--holder-attempt", type=int, default=45)
    ap.add_argument("--median-hold", type=int, default=20)
    ap.add_argument("--cooldown-min", type=int, default=120)
    ap.add_argument("--dry-run", action="store_true", help="never send")
    a = ap.parse_args()

    now = time.time()
    holds, landed = holds_by_repo(now - 6 * 3600)
    try:
        cli_mtime = os.stat(os.path.join(HOME, ".local", "bin", "amux")).st_mtime
    except OSError:
        cli_mtime = None
    try:
        state = json.load(open(STATE))
    except Exception:
        state = {}

    found = []
    queues = sorted(glob.glob(os.path.join(LOCKS, "land-*.q")))
    for qd in queues:
        key = os.path.basename(qd)[len("land-"):-len(".q")]
        lock = qd[:-2]
        tickets = sorted(os.listdir(qd)) if os.path.isdir(qd) else []
        if not tickets and not os.path.isdir(lock):
            continue
        waits, stale, lanes = [], [], []
        for t in tickets:
            pid = t.rsplit("-", 1)[-1]
            who = (open(os.path.join(qd, t), errors="replace").readline().strip() or "?")
            lanes.append(who)
            if t.startswith("000"):
                arrived = int(now // 1e9 * 1e9 + int(t[3:12]))
            else:
                arrived = int(t[:12])
            waits.append((now - arrived) / 60)
            st = ps_start(pid)
            # 30 minutes of slack: every CLI install would otherwise mark
            # every waiter stale; a land started well before it is the real
            # case (the 18:32Z GH_TOKEN fix reached no land started earlier).
            if cli_mtime and st and st < cli_mtime - 1800:
                stale.append(f"{who} (pid {pid})")
        holder, attempt = None, None
        if os.path.isdir(lock):
            holder = (open(os.path.join(lock, "who")).read().strip() if os.path.exists(os.path.join(lock, "who")) else "?")
            try:
                attempt = (now - int(open(os.path.join(lock, "since")).read().strip())) / 60
            except Exception:
                attempt = None
        h = holds.get(key, [])
        med = statistics.median(h) if h else None
        n_land = landed.get(key, 0)
        rate = n_land / 6.0
        depth = len(tickets)
        # Only with at least 3 measured holds: land.log lines carry the repo
        # key since 2026-10-01 17:38Z, and fewer holds undercount the rate.
        drain = (depth / rate) if rate > 0 and len(h) >= 3 else None
        why = []
        if depth >= a.depth: why.append(f"depth {depth} >= {a.depth}")
        if waits and max(waits) >= a.oldest_wait: why.append(f"oldest wait {max(waits):.0f} min >= {a.oldest_wait}")
        if attempt is not None and attempt >= a.holder_attempt: why.append(f"holder attempt {attempt:.0f} min >= {a.holder_attempt}")
        if med is not None and med >= a.median_hold and depth >= 4: why.append(f"median hold {med:.0f} min >= {a.median_hold} with {depth} waiting")
        rec = {"ts": int(now), "repo_key": key, "depth": depth,
               "oldest_wait_min": round(max(waits), 1) if waits else 0,
               "holder": holder, "holder_attempt_min": None if attempt is None else round(attempt, 1),
               "median_hold_min": None if med is None else round(med, 1), "holds_measured_6h": len(h),
               "landed_6h": n_land, "drain_h": None if drain is None else round(drain, 1),
               "stale_code_waiters": stale, "lanes": sorted(set(lanes)),
               "measured": True, "n_considered": depth}
        if why:
            rec.update({"verdict": "pipeline_bottleneck", "why": why})
            found.append(rec)
        else:
            rec.update({"verdict": "pipeline_ok"})
        with open(OUT, "a") as f:
            f.write(json.dumps(rec) + "\n")
        print(json.dumps(rec))

    if not queues:
        rec = {"ts": int(now), "verdict": "no_queues", "measured": True, "n_considered": 0}
        with open(OUT, "a") as f:
            f.write(json.dumps(rec) + "\n")
        print(json.dumps(rec))

    for rec in found:
        key = rec["repo_key"]
        last = state.get(key, {})
        worse = (rec["drain_h"] or 0) >= 1.5 * (last.get("drain_h") or 0) and (rec["drain_h"] or 0) > 0
        due = now - last.get("sent", 0) >= a.cooldown_min * 60
        if not a.route or a.dry_run or not (due or worse):
            continue
        levers = []
        if rec["stale_code_waiters"]:
            levers.append("re-queue lands running old code (amux land --cancel, then amux land --detach) so they get the current fixes: "
                          + ", ".join(rec["stale_code_waiters"][:8]))
        if rec["median_hold_min"] and rec["median_hold_min"] >= a.median_hold:
            levers.append(f"the gate itself: median hold {rec['median_hold_min']} min is the per-landing cost; shortening it moves every lane")
        if rec["holder_attempt_min"] and rec["holder_attempt_min"] >= a.holder_attempt:
            levers.append(f"the holder {rec['holder']} is {rec['holder_attempt_min']:.0f} min into one attempt: check its push is progressing")
        msg = (f"Bottleneck (amux pipeline-bottlenecks, AH-291): the land queue {key} is the constraint. "
               f"{'; '.join(rec['why'])}. Depth {rec['depth']}, oldest wait {rec['oldest_wait_min']:.0f} min, "
               f"holder {rec['holder']} ({rec['holder_attempt_min']} min), median hold {rec['median_hold_min']} min over "
               f"{rec['holds_measured_6h']} holds, {rec['landed_6h']} landings in 6 h, "
               + (f"drain about {rec['drain_h']} h.\n" if rec['drain_h'] is not None else "drain unmeasured (under 3 measured holds).\n")
               + ("Levers: " + " | ".join(levers) if levers else "No lever is measurable from the queue alone."))
        r = subprocess.run(["amux", "send", a.route, "--stdin"], input=msg, capture_output=True, text=True)
        sent = r.returncode == 0
        with open(OUT, "a") as f:
            f.write(json.dumps({"ts": int(now), "verdict": "pipeline_bottleneck_routed" if sent else "pipeline_bottleneck_route_failed",
                                "repo_key": key, "route": a.route, "measured": True, "n_considered": 1,
                                "detail": (r.stdout or r.stderr).strip()[:200]}) + "\n")
        if sent:
            state[key] = {"sent": now, "drain_h": rec["drain_h"]}
    json.dump(state, open(STATE, "w"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
