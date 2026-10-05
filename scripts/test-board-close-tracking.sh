#!/usr/bin/env bash
# test-board-close-tracking.sh: board-close-tracking closes a tracking card only
# when every non-discarded lane card naming it is verified (at least one), never
# when one is still open, never when the tracking card is tagged held or already
# terminal, and stamps the evidence with the lane card ids.
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d "${TMPDIR:-/tmp}/bct.XXXXXX")"
python3 - "$T/b.json" <<'PY'
import json, sys
L = "Tracking card on the orchestrator's board: "
cards = [
  {"id": "MO-1", "session": "orch", "status": "done"},
  {"id": "MO-2", "session": "orch", "status": "backlog"},
  {"id": "MO-3", "session": "orch", "status": "done", "tags": ["held"]},
  {"id": "MO-4", "session": "orch", "status": "verified"},
  {"id": "MO-5", "session": "orch", "status": "todo"},
  {"id": "A-1", "session": "lane-a", "status": "verified", "desc": L + "MO-1", "evidence": "a1 proof"},
  {"id": "B-1", "session": "lane-b", "status": "verified", "desc": L + "MO-1", "evidence": "b1 proof"},
  {"id": "B-2", "session": "lane-b", "status": "discarded", "desc": L + "MO-1"},
  {"id": "A-2", "session": "lane-a", "status": "verified", "desc": L + "MO-2"},
  {"id": "A-3", "session": "lane-a", "status": "todo", "desc": L + "MO-2"},
  {"id": "A-4", "session": "lane-a", "status": "verified", "desc": L + "MO-3"},
  {"id": "A-5", "session": "lane-a", "status": "verified", "desc": L + "MO-4"},
  {"id": "A-6", "session": "lane-a", "status": "discarded", "desc": L + "MO-5"},
  {"id": "MO-6", "session": "orch", "status": "done", "title": "GS12 5.11 an item"},
  {"id": "A-7", "session": "lane-a", "status": "verified", "desc": L + "MO-6"},
  {"id": "A-8", "session": "lane-a", "status": "backlog", "title": "GS12 5.11b the open clause"},
  {"id": "MO-7", "session": "orch", "status": "done", "title": "GS12 5.1 another item"},
  {"id": "A-9", "session": "lane-a", "status": "verified", "desc": L + "MO-7"},
  {"id": "A-10", "session": "lane-a", "status": "todo", "title": "GS12 5.110 an unrelated item"},
]
json.dump(cards, open(sys.argv[1], "w"))
PY
out="$(python3 "$ROOT/scripts/board-close-tracking.py" --orchestrator orch --board-file "$T/b.json")"
fail=0
printf '%s\n' "$out" | grep -q '2 tracking card(s) ready' && echo "ok   exactly two tracking cards are ready" || { echo "FAIL: $out"; fail=1; }
printf '%s\n' "$out" | grep -q 'MO-1 <- A-1, B-1' && echo "ok   all named lane cards verified (a discarded one ignored) closes it" || { echo "FAIL MO-1: $out"; fail=1; }
printf '%s\n' "$out" | grep -q 'MO-2' && { echo "FAIL a todo lane card did not block MO-2"; fail=1; } || echo "ok   an open lane card blocks it"
printf '%s\n' "$out" | grep -q 'MO-3' && { echo "FAIL a held card was closed"; fail=1; } || echo "ok   a card tagged held is skipped"
printf '%s\n' "$out" | grep -q 'MO-5' && { echo "FAIL a card whose only lane card is discarded was closed"; fail=1; } || echo "ok   no live lane card means no close"
ev="$(python3 -c "
import importlib.util,json,sys
s=importlib.util.spec_from_file_location('b','$ROOT/scripts/board-close-tracking.py'); m=importlib.util.module_from_spec(s); s.loader.exec_module(m)
c=json.load(open('$T/b.json')); print(m.evidence_for(['A-1','B-1'],{x['id']:x for x in c},'2026-10-05T00:00:00Z'))")"
printf '%s' "$ev" | grep -q 'auto-verified from A-1, B-1 at 2026-10-05T00:00:00Z' && printf '%s' "$ev" | grep -q 'b1 proof' && echo "ok   the evidence is stamped with the lane ids and carries theirs" || { echo "FAIL evidence: $ev"; fail=1; }
printf '%s\n' "$out" | grep -q 'MO-6' && { echo "FAIL a plan-number lane card (5.11b, backlog) did not block MO-6"; fail=1; } || echo "ok   a lane card carrying only the plan number blocks it"
printf '%s\n' "$out" | grep -q 'MO-7 <- A-9$' && echo "ok   5.1 does not match 5.110" || { echo "FAIL MO-7: $out"; fail=1; }
rm -rf -- "${T:?}"
exit $fail
