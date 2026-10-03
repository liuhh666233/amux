#!/usr/bin/env bash
# test-orch-pace-proof.sh: before its checkpoint the proof verdict projects the
# 6h proof rate to the checkpoint, so 6 of 66 with 33 due in 4.5 hours reads
# BEHIND rather than ON PACE (2026-10-03), and a fast enough rate reads ON PACE.
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
H="$(mktemp -d "${TMPDIR:-/tmp}/opp.XXXXXX")"; mkdir -p "$H/.amux/logs"
mk() { python3 - "$1" "$2" > "$H/board.json" <<'PY'
import json,sys
fast=int(sys.argv[1]); now=float(sys.argv[2])
cards=[]
for i in range(66):
    v=i<6+fast
    cards.append({"id":f"P-{i}","session":"orch","title":f"GS12 proof {i}","status":"verified" if v else "todo",
                  "closed_at": (now-3600 if i>=6 else now-86400) if v else None})
print(json.dumps(cards))
PY
}
now=2026-10-03T23:29:00+00:00; ts=$(python3 -c "import datetime;print(datetime.datetime.fromisoformat('$now').timestamp())")
run() { HOME="$H" python3 "$ROOT/scripts/orch-pace.py" --orchestrator orch --lane-prefix gsx- --deadline 2026-10-04T23:59:00-04:00 --proof-prefix "GS12 proof" --board-file "$H/board.json" --now "$now" | grep '^proof:'; }
fail=0
mk 0 "$ts"; out="$(run)"
printf '%s' "$out" | grep -q 'BEHIND' && echo "ok   6 of 66 with no recent proofs and 33 due in 4.5h reads BEHIND" || { echo "FAIL $out"; fail=1; }
mk 24 "$ts"; out="$(run)"
printf '%s' "$out" | grep -q 'ON PACE' && echo "ok   24 proofs in the last 6h (4/h) projects past 33 and reads ON PACE" || { echo "FAIL $out"; fail=1; }
rm -rf -- "${H:?}"
exit $fail
