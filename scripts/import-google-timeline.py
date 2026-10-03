#!/usr/bin/env python3
"""Import a Google Maps Timeline export (on-device Timeline.json) into amux
location history (/api/map/location, AMUX-5458).

Mapping, Google -> amux:
  timelinePath[].point         -> location_points (ts = startTime + offset min)
  activity.start / .end        -> location_points at the segment's start/end
  visit (hierarchyLevel 0)     -> location_visits, plus a point at arrival and
                                  departure so the stop segmenter sees the dwell
  activity.topCandidate.type   -> point `activity` in Core Motion's vocabulary
                                  (walking/running/cycling/automotive/stationary)

Level-1 visits are skipped: they nest inside a level-0 visit and would draw a
second, overlapping stop. Everything is stored with device and source
`google-timeline`, so it can be told apart from the phone's own capture and
removed with DELETE /api/map/location/points/range.

Points at or after the first point the phone recorded itself are dropped by
default: the phone's live capture is denser and more accurate, and two sources
interleaved over the same minutes make jittery trips. --no-cutoff keeps them.

IDs are a hash of (kind, ts, lat, lon), so re-running the import is a no-op.

Usage:
  scripts/import-google-timeline.py Timeline.json [--dry-run] [--no-cutoff]
"""
import argparse
import bisect
import datetime as dt
import hashlib
import json
import os
import ssl
import subprocess
import sys
import urllib.request

DEVICE = SOURCE = "google-timeline"
BATCH = 5000  # server MAX_BATCH

MODE = {
    "walking": "walking", "running": "running", "cycling": "cycling",
    "in passenger vehicle": "automotive", "in vehicle": "automotive",
    "driving": "automotive", "in taxi": "automotive", "motorcycling": "automotive",
    "in bus": "automotive", "in subway": "automotive", "in train": "automotive",
    "in tram": "automotive", "in ferry": "automotive",
}


def ts(s):
    return dt.datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()


def geo(s):
    lat, lon = s.removeprefix("geo:").split(",")
    return float(lat), float(lon)


def conf(p):
    try:
        p = float(p)
    except (TypeError, ValueError):
        return None
    return "high" if p >= 0.8 else "medium" if p >= 0.5 else "low"


def pid(kind, t, lat, lon):
    h = hashlib.sha1(f"{kind}|{t:.3f}|{lat:.6f}|{lon:.6f}".encode()).hexdigest()[:24]
    return f"gt-{kind}-{h}"


def convert(entries):
    points, visits = {}, []
    # (start, end, activity, conf) spans used to label path points.
    spans = []

    def add(kind, t, lat, lon, activity=None, activity_conf=None):
        i = pid(kind, t, lat, lon)
        points[i] = {"id": i, "ts": t, "lat": lat, "lon": lon, "activity": activity,
                     "activity_conf": activity_conf, "source": SOURCE}

    for e in entries:
        if "visit" in e:
            v = e["visit"]
            top = v.get("topCandidate", {})
            if v.get("hierarchyLevel", "0") != "0" or "placeLocation" not in top:
                continue
            a, d = ts(e["startTime"]), ts(e["endTime"])
            lat, lon = geo(top["placeLocation"])
            key = top.get("placeID") or f"{lat:.6f},{lon:.6f}"
            visits.append({"id": f"gt-visit-{hashlib.sha1(f'{key}|{a:.3f}'.encode()).hexdigest()[:24]}",
                           "arrival": a, "departure": d, "lat": lat, "lon": lon})
            c = conf(v.get("probability"))
            add("visit", a, lat, lon, "stationary", c)
            add("visit", d, lat, lon, "stationary", c)
            spans.append((a, d, "stationary", c))
        elif "activity" in e:
            act = e["activity"]
            top = act.get("topCandidate", {})
            mode = MODE.get(top.get("type", ""), "unknown")
            c = conf(top.get("probability"))
            a, d = ts(e["startTime"]), ts(e["endTime"])
            spans.append((a, d, mode, c))
            if "start" in act:
                add("act", a, *geo(act["start"]), mode, c)
            if "end" in act:
                add("act", d, *geo(act["end"]), mode, c)

    spans.sort()
    starts = [s[0] for s in spans]

    def label(t):
        k = bisect.bisect_right(starts, t) - 1
        # Look back a few spans: visits and activities can overlap at edges.
        for j in range(k, max(k - 4, -1), -1):
            s, en, mode, c = spans[j]
            if s <= t <= en:
                return mode, c
        return None, None

    for e in entries:
        if "timelinePath" not in e:
            continue
        base = ts(e["startTime"])
        for p in e["timelinePath"]:
            t = base + float(p["durationMinutesOffsetFromStartTime"]) * 60
            lat, lon = geo(p["point"])
            add("path", t, lat, lon, *label(t))

    return sorted(points.values(), key=lambda p: p["ts"]), visits


def amux_url():
    return subprocess.run(["amux", "url"], capture_output=True, text=True, check=True).stdout.strip()


def post(url, token, body):
    req = urllib.request.Request(url, data=json.dumps(body).encode(), method="POST",
                                 headers={"Content-Type": "application/json",
                                          "Authorization": f"Bearer {token}"})
    ctx = ssl._create_unverified_context()
    with urllib.request.urlopen(req, context=ctx, timeout=120) as r:
        return json.load(r)


def get(url, token):
    req = urllib.request.Request(url, headers={"Authorization": f"Bearer {token}"})
    with urllib.request.urlopen(req, context=ssl._create_unverified_context(), timeout=60) as r:
        return json.load(r)


def first_phone_ts(base, token):
    """Earliest point NOT from this importer, paging past our own rows."""
    summary = get(f"{base}/api/map/location/summary", token)
    if not any(d["device"] != DEVICE for d in summary.get("devices", [])):
        return None
    frm, to = 946_684_800, dt.datetime.now().timestamp() + 86_400
    while True:
        rows = get(f"{base}/api/map/location/points?from={frm}&to={to}&limit=50000", token)["points"]
        for r in rows:
            if r["device"] != DEVICE:
                return r["ts"]
        if len(rows) < 50000:
            return None
        frm = rows[-1]["ts"]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("file")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--no-cutoff", action="store_true")
    args = ap.parse_args()

    entries = json.load(open(args.file))
    if not isinstance(entries, list):
        sys.exit("expected the on-device Timeline.json export: a top-level JSON array")
    points, visits = convert(entries)
    print(f"converted: {len(points)} points, {len(visits)} visits from {len(entries)} entries")

    base = amux_url()
    token = open(os.path.expanduser("~/.amux/auth_token")).read().strip()

    cutoff = None
    if not args.no_cutoff:
        cutoff = first_phone_ts(base, token)
    if cutoff:
        before = len(points), len(visits)
        points = [p for p in points if p["ts"] < cutoff]
        visits = [v for v in visits if v["arrival"] < cutoff]
        print(f"cutoff {dt.datetime.fromtimestamp(cutoff).isoformat(timespec='seconds')} "
              f"(first phone-recorded point): dropped {before[0] - len(points)} points, "
              f"{before[1] - len(visits)} visits")

    if points:
        span = (dt.datetime.fromtimestamp(points[0]["ts"]).date(), dt.datetime.fromtimestamp(points[-1]["ts"]).date())
        print(f"range: {span[0]} .. {span[1]}")
    if args.dry_run:
        print("dry run: nothing written")
        return

    acc = dup = rej = 0
    for i in range(0, len(points), BATCH):
        r = post(f"{base}/api/map/location/points", token, {"device": DEVICE, "points": points[i:i + BATCH]})
        if not r.get("ok"):
            sys.exit(f"points batch {i} refused: {r}")
        acc, dup, rej = acc + r["accepted"], dup + r["duplicate"], rej + len(r["rejected"])
        for x in r["rejected"][:5]:
            print("  rejected", x)
    print(f"points: {acc} accepted, {dup} duplicate, {rej} rejected")
    vs = vr = 0
    for i in range(0, len(visits), BATCH):
        r = post(f"{base}/api/map/location/visits", token, {"device": DEVICE, "visits": visits[i:i + BATCH]})
        if not r.get("ok"):
            sys.exit(f"visits batch {i} refused: {r}")
        vs, vr = vs + r["stored"], vr + r["rejected"]
    print(f"visits: {vs} stored, {vr} rejected")


if __name__ == "__main__":
    main()
