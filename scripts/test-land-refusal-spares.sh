#!/usr/bin/env bash
# test-land-refusal-spares.sh [amux]: an anonymous batch refusal counts against
# a waiter only when it names a file that waiter's commits touched, or names no
# file. gates' 13-lane batch at 01:08Z on 2026-10-03 was refused by a defect
# already on origin (server/scripts/ops/golden_known_item.py, named on an
# indented line after a blank one) and 12 innocent tickets went solo.
set -uo pipefail
AM="${1:-$(cd "$(dirname "$0")/.." && pwd)/amux}"
F="$(mktemp "${TMPDIR:-/tmp}/rs.XXXXXX")"
sed -n '/^_land_refusal_spares() {/,/^}/p' "$AM" > "$F"
# shellcheck disable=SC1090
. "$F"; rm -f "$F"
T="$(mktemp -d "${TMPDIR:-/tmp}/rs.XXXXXX")"; cd "$T" || exit 1
git init -q; git config user.email t@t; git config user.name t
mkdir -p server/scripts/ops server/api; echo a > server/api/x.py; echo b > server/scripts/ops/golden_known_item.py
git add -A; git commit -qm base; base="$(git rev-parse HEAD)"
echo c >> server/api/x.py; git commit -qm innocent -- server/api/x.py; innocent="$(git rev-parse HEAD)"
git checkout -q "$base"; echo d >> server/scripts/ops/golden_known_item.py; git commit -qm culprit -- server/scripts/ops/golden_known_item.py; culprit="$(git rev-parse HEAD)"
cat > "$T/hook.log" <<'EOF'
pre-push: gating candidate commit abc on https://github.com/mixpeek/mixpeek.git
✗ 1 of 54 spec schemas are defined ONLY in files the `generate-openapi` hook does not watch.
  Editing them changes openapi.json and fires no hook.

       1 schema(s)  server/scripts/ops/golden_known_item.py

  Current pattern: ^server/(api|shared)/.*\.py$
EOF
printf 'qdrant-refs: ✗ REFUSED\n' > "$T/nofile.log"
res() { echo "excluded: the batch was refused by the push hook (full output $1); landing on your own turn"; }
fail=0
_land_refusal_spares "$(res "$T/hook.log")" "$innocent" "$base" && echo "ok   a waiter that did not touch the named file is spared" || { echo "FAIL innocent waiter counted"; fail=1; }
_land_refusal_spares "$(res "$T/hook.log")" "$culprit" "$base" && { echo "FAIL the waiter that touched the named file was spared"; fail=1; } || echo "ok   the waiter that touched the named file is counted"
_land_refusal_spares "$(res "$T/nofile.log")" "$innocent" "$base" && { echo "FAIL a refusal naming no file spared a waiter"; fail=1; } || echo "ok   a refusal naming no file still counts"
_land_refusal_spares "excluded: no output path here" "$innocent" "$base" && { echo "FAIL an unreadable refusal spared a waiter"; fail=1; } || echo "ok   no readable output still counts"
cd / && rm -rf -- "${T:?}"
exit $fail
