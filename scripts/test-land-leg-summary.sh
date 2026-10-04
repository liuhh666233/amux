#!/usr/bin/env bash
# test-land-leg-summary.sh [amux]: a pre-push leg prints its verdict when it
# finishes, so the "slowest legs" summary must credit a gap to the line that
# ENDS it. Land .45 credited it to the line before, and on 2026-10-04 reported
# "session-stamp 26s" for env-census's 26 s (gs12-gates, GG-63).
set -uo pipefail
AM="${1:-$(cd "$(dirname "$0")/.." && pwd)/amux}"
T="$(mktemp -d "${TMPDIR:-/tmp}/lls.XXXXXX")"
cat > "$T/timed.log" <<'EOF'
     4.6s pre-push: gating candidate commit be8b29ab09
    20.1s commit-identity: OK
    20.2s session-stamp: outgoing 1 = 1 gs12-gates
    46.6s env-census: OK (no prefix past its ceiling)
    47.5s codegen-copies: OK
    75.2s zb-render: OK
    75.8s stale-revert: auditing 13 modified file(s)
   100.9s stale-revert: OK
EOF
sed -n '/^  _land_leg_summary() {/,/^  }/p' "$AM" > "$T/fn.sh"
fail=0
[ -s "$T/fn.sh" ] || { echo "FAIL _land_leg_summary not found"; exit 1; }
out="$(bash -c "_land_log() { printf '%s\n' \"\$*\"; }; . '$T/fn.sh'; _land_leg_summary '$T/timed.log' 101")"
printf '%s' "$out" | grep -q 'env-census 26s' && echo "ok   the gap is credited to the leg whose line ends it" || { echo "FAIL $out"; fail=1; }
printf '%s' "$out" | grep -q 'session-stamp' && { echo "FAIL a 0.1 s leg is named: $out"; fail=1; } || echo "ok   the fast leg printed just before is not named"
printf '%s' "$out" | grep -q 'stale-revert 25s\|stale-revert 26s' && echo "ok   a two-line leg runs from before its first line to its last" || { echo "FAIL $out"; fail=1; }
rm -rf -- "${T:?}"
exit $fail
