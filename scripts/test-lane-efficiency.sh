#!/usr/bin/env bash
# test-lane-efficiency.sh: lane-efficiency.py puts each lane's token cost
# beside its verified cards, pushes and done pile, and computes $/verified;
# a lane with no cost rows says so rather than reading as free.
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d "${TMPDIR:-/tmp}/leff.XXXXXX")"
NOW=1791140000
python3 - "$T" "$NOW" <<'PY'
import json, sqlite3, sys
t, now = sys.argv[1], int(sys.argv[2])
db = sqlite3.connect(f"{t}/a.db")
db.execute("CREATE TABLE token_ledger (ts INTEGER, session TEXT, cost_usd REAL)")
db.executemany("INSERT INTO token_ledger VALUES (?,?,?)",
               [(now - 100, "gs12-a", 300.0), (now - 200, "gs12-a", 100.0), (now - 90000, "gs12-a", 999.0), (now - 100, "other", 50.0)])
db.commit()
json.dump([
    {"id": "A-1", "session": "gs12-a", "status": "verified", "entered_state_at": now - 300},
    {"id": "A-2", "session": "gs12-a", "status": "verified", "entered_state_at": now - 400},
    {"id": "A-3", "session": "gs12-a", "status": "done"},
    {"id": "B-1", "session": "gs12-b", "status": "verified", "entered_state_at": now - 300},
    {"id": "X-1", "session": "other", "status": "verified", "entered_state_at": now - 300},
], open(f"{t}/b.json", "w"))
PY
iso() { python3 -c "import time,sys;print(time.strftime('%Y-%m-%dT%H:%M:%SZ',time.gmtime(int(sys.argv[1]))))" "$1"; }
printf '%s gs12-a landed abc123 on main (attempt 1)\n%s gs12-b landed in a batch: def on main\n' \
  "$(iso $((NOW-50)))" "$(iso $((NOW-60)))" > "$T/land.log"
out="$(python3 "$ROOT/scripts/lane-efficiency.py" --db "$T/a.db" --board-file "$T/b.json" --land-log "$T/land.log" --now "$NOW" --hours 24)"
fail=0
printf '%s\n' "$out" | grep -qE '^gs12-a +400 +2 +1 +1 +200$' && echo "ok   cost in window, verified, pushes, done and \$/verified for a lane" || { echo "FAIL gs12-a row: $out"; fail=1; }
printf '%s\n' "$out" | grep -qE '^gs12-b +no rows +1 +1 +0 +-$' && echo "ok   a lane with no cost rows says so" || { echo "FAIL gs12-b row: $out"; fail=1; }
printf '%s\n' "$out" | grep -q '^other' && { echo "FAIL a lane outside the prefix is listed"; fail=1; } || echo "ok   lanes outside the prefix are left out"
rm -rf -- "${T:?}"
exit $fail
