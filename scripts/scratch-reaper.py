#!/usr/bin/env python3
"""scratch-reaper: free Claude Code session scratch whose conversation has ended.

/private/tmp/claude-<uid>/<project>/<conversation-id>/ is each conversation's
scratch (worktrees, test repos, build outputs). On 2026-10-01 it held 355 GB on a
disk at 97 percent, and the Mixpeek land gate's export steps stalled under it
(mixpeek-override). Nothing removed scratch when a conversation ended.

A scratch dir is reaped only when ALL hold:
  - no amux session records its conversation id (cc_conversation_id in
    ~/.amux/sessions/*.meta.json): a lane can still resume it;
  - no running process names the id on its command line (--resume <id>) or has
    its cwd inside the dir;
  - its transcript (~/.claude/projects/<project>/<id>.jsonl) has not changed for
    --idle-days (default 3), or does not exist and the dir itself is that old.
"No live process" alone is not "unused": a resumable conversation's scratch is
kept by the first rule.

Size is counted in UNIQUE bytes (files with one link): local clones share pack
files with ~/Dev/mixpeek/.git by hard link, so du counted 18.9 GB of packs once
per clone and overstated what deletion frees (mixpeek-homepage-claude,
2026-10-01: its du 22 GB, unique 1.08 GB).

Default is a dry run that prints candidates; --apply deletes them. Every
decision is one JSON line in ~/.amux/logs/scratch-reaper.jsonl
(verdict=scratch_reaped|scratch_kept, measured, n_considered).
"""
import argparse, glob, json, os, shutil, subprocess, sys, time

ap = argparse.ArgumentParser()
ap.add_argument("--root", default=f"/private/tmp/claude-{os.getuid()}")
ap.add_argument("--idle-days", type=float, default=3)
ap.add_argument("--apply", action="store_true")
ap.add_argument("--max-dirs", type=int, default=100000)
a = ap.parse_args()

HOME = os.path.expanduser("~")
LOG = os.path.join(HOME, ".amux", "logs", "scratch-reaper.jsonl")
now = time.time()
idle_s = a.idle_days * 86400

# 1. Conversations amux can still resume.
resumable = set()
for f in glob.glob(os.path.join(HOME, ".amux", "sessions", "*.meta.json")):
    try:
        cid = json.load(open(f)).get("cc_conversation_id") or ""
        if cid:
            resumable.add(cid)
    except Exception:
        pass

# 2. Conversations a running process names, and cwds of running processes.
ps = subprocess.run(["ps", "-Ao", "pid=,command="], capture_output=True, text=True).stdout
named = set()
for line in ps.splitlines():
    for tok in line.split():
        if len(tok) == 36 and tok.count("-") == 4:
            named.add(tok)
cwds = set()
lsof = subprocess.run(["lsof", "-a", "-d", "cwd", "-Fn", "-u", str(os.getuid())], capture_output=True, text=True).stdout
for line in lsof.splitlines():
    if line.startswith("n"):
        cwds.add(line[1:])

cands, kept, n, cand_bytes = [], 0, 0, []
for proj in sorted(glob.glob(os.path.join(a.root, "*"))):
    if not os.path.isdir(proj):
        continue
    for d in sorted(glob.glob(os.path.join(proj, "*"))):
        cid = os.path.basename(d)
        if not (len(cid) == 36 and cid.count("-") == 4 and os.path.isdir(d)):
            continue
        n += 1
        if n > a.max_dirs:
            break
        why = None
        if cid in resumable:
            why = "resumable (an amux session records this conversation)"
        elif cid in named:
            why = "a running process names it"
        elif any(c == d or c.startswith(d + "/") for c in cwds):
            why = "a running process has its cwd inside"
        else:
            t = os.path.join(HOME, ".claude", "projects", os.path.basename(proj), cid + ".jsonl")
            try:
                last = os.stat(t).st_mtime
            except OSError:
                last = os.stat(d).st_mtime
            if now - last < idle_s:
                why = f"active within {a.idle_days:g} days"
        rec = {"ts": int(now), "dir": d, "measured": True, "n_considered": 1}
        if not why:
            uniq = 0
            for root, _, files in os.walk(d):
                for fn in files:
                    try:
                        st = os.lstat(os.path.join(root, fn))
                        if st.st_nlink == 1:
                            uniq += st.st_size
                    except OSError:
                        pass
            rec["unique_bytes"] = uniq
        if why:
            kept += 1
            rec.update({"verdict": "scratch_kept", "why": why})
        else:
            cands.append(d)
            cand_bytes.append(rec.get("unique_bytes", 0))
            rec.update({"verdict": "scratch_reaped" if a.apply else "scratch_candidate"})
            if a.apply:
                shutil.rmtree(d, ignore_errors=True)
                rec["removed"] = not os.path.exists(d)
        with open(LOG, "a") as f:
            f.write(json.dumps(rec) + "\n")

summary = {"ts": int(now), "verdict": "scratch_reaper_run", "apply": a.apply, "considered": n,
           "candidate_unique_gb": round(sum(r for r in cand_bytes) / 1073741824, 2),
           "kept": kept, "candidates": len(cands), "measured": True, "n_considered": n}
with open(LOG, "a") as f:
    f.write(json.dumps(summary) + "\n")
by_proj = {}
for d in cands:
    p = os.path.basename(os.path.dirname(d))
    by_proj[p] = by_proj.get(p, 0) + 1
print(json.dumps(summary))
for p, c in sorted(by_proj.items(), key=lambda x: -x[1])[:15]:
    print(f"  {c:4d}  {p}")
