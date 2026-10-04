#!/usr/bin/env bash
# test-mac-cleanup-lane-tmp.sh: the cleanup tick's reap_lane_tmp removes
# ~/.amux/tmp/<worker>/<entry> only when nothing inside changed past the floor
# and no process works inside it (2026-10-04: 101 GB idle in lane TMPDIRs).
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d "${TMPDIR:-/tmp}/lt.XXXXXX")"; R="$T/root"
mkdir -p "$R/lane1/oldidle/sub" "$R/lane1/fresh" "$R/lane1/oldtopfreshinside/deep" "$R/lane2/inuse"
echo x > "$R/lane1/oldidle/sub/f"; echo y > "$R/lane1/oldtopfreshinside/deep/new"
touch -t 202609300000 "$R/lane1/oldidle/sub/f" "$R/lane1/oldidle/sub" "$R/lane1/oldidle" "$R/lane1/oldtopfreshinside" "$R/lane2/inuse"
(cd "$R/lane2/inuse" && exec sleep 30) & holder=$!
sleep 1
F="$(mktemp "${TMPDIR:-/tmp}/ltf.XXXXXX")"
sed -n '/^reap_lane_tmp() {/,/^}/p' "$ROOT/scripts/mac-cleanup-tick.sh" > "$F"
fail=0
[ -s "$F" ] || { echo "FAIL reap_lane_tmp is not in the tick"; exit 1; }
out="$(LANE_TMP_ROOT="$R" LANE_TMP_IDLE_MIN=1440 bash -c ". '$F'; reap_lane_tmp 0")"
[ ! -e "$R/lane1/oldidle" ] && echo "ok   an entry with nothing changed in a day is removed" || { echo "FAIL old idle entry kept"; fail=1; }
[ -d "$R/lane1/fresh" ] && echo "ok   a fresh entry is kept" || { echo "FAIL fresh entry removed"; fail=1; }
[ -d "$R/lane1/oldtopfreshinside" ] && echo "ok   an old directory with a fresh file deep inside is kept" || { echo "FAIL in-use cargo-style dir removed"; fail=1; }
[ -d "$R/lane2/inuse" ] && echo "ok   an old entry a process works in is kept" || { echo "FAIL cwd entry removed"; fail=1; }
printf '%s' "$out" | grep -q 'lane tmp: removed 1 entry' && echo "ok   the tick logs the count" || { echo "FAIL log: $out"; fail=1; }
mkdir -p "$R/lane3/a" "$R/lane3/b" "$R/lane3/c"; touch -t 202609300000 "$R/lane3/a" "$R/lane3/b" "$R/lane3/c"
out2="$(LANE_TMP_ROOT="$R" LANE_TMP_IDLE_MIN=1440 LANE_TMP_MAX=2 bash -c ". '$F'; reap_lane_tmp 0")"
left=$(ls "$R/lane3" | wc -l | tr -d ' ')
[ "$left" = 1 ] && printf '%s' "$out2" | grep -q 'capped at 2' && echo "ok   a pass removes at most LANE_TMP_MAX entries and says it was capped" || { echo "FAIL cap: left=$left out=$out2"; fail=1; }
grep -q 'AMUX_CLEANUP_LANE_TMP=0' "$ROOT/scripts/test-mac-cleanup-tick.sh" && echo "ok   the full-tick test never reaps the live lane temp dirs" || { echo "FAIL full-tick test does not disable the lane reaper"; fail=1; }
kill "$holder" 2>/dev/null; wait 2>/dev/null
rm -f "$F"; rm -rf -- "${T:?}"
exit $fail
