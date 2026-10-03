#!/usr/bin/env bash
# test-mac-cleanup-keep-context.sh: after the cleanup tick stops an idle colima
# VM, the profile's docker context still exists (colima stop deletes it, and a
# VM restarted any way but `colima start` left lanes on "context not found":
# gs12-tiering, 2026-10-03). Runs keep_docker_context against a stub docker.
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d "${TMPDIR:-/tmp}/kc.XXXXXX")"
mkdir -p "$T/bin"
cat > "$T/bin/docker" <<'EOF'
#!/usr/bin/env bash
# stub: contexts live as files in $KC_DIR
case "$1 $2" in
  "context inspect") [ -f "$KC_DIR/$3" ] ;;
  "context create") printf '%s\n' "$*" > "$KC_DIR/$3" ;;
  *) exit 0 ;;
esac
EOF
chmod +x "$T/bin/docker"; mkdir -p "$T/ctx" "$T/state"
F="$(mktemp "${TMPDIR:-/tmp}/kcf.XXXXXX")"
sed -n '/^keep_docker_context() {/,/^}/p' "$ROOT/scripts/mac-cleanup-tick.sh" > "$F"
fail=0
[ -s "$F" ] || { echo "FAIL keep_docker_context is not in mac-cleanup-tick.sh"; exit 1; }
grep -q 'keep_docker_context "\$p"' "$ROOT/scripts/mac-cleanup-tick.sh" && echo "ok   the stop path calls keep_docker_context" || { echo "FAIL the stop path does not call it"; fail=1; }
out="$(PATH="$T/bin:$PATH" KC_DIR="$T/ctx" STATE_DIR="$T/state" COLIMA_HOME="$T/colima" bash -c ". '$F'; keep_docker_context goal-shared")"
[ -f "$T/ctx/colima-goal-shared" ] && grep -q "host=unix://$T/colima/goal-shared/docker.sock" "$T/ctx/colima-goal-shared" \
  && echo "ok   a missing context is recreated at the VM's socket" || { echo "FAIL context not recreated: $out"; fail=1; }
grep -q 'kept docker context colima-goal-shared' "$T/state/vm-stops.log" && echo "ok   vm-stops.log records it" || { echo "FAIL no log line"; fail=1; }
out2="$(PATH="$T/bin:$PATH" KC_DIR="$T/ctx" STATE_DIR="$T/state" COLIMA_HOME="$T/colima" bash -c ". '$F'; keep_docker_context goal-shared")"
[ -z "$out2" ] && echo "ok   an existing context is left alone" || { echo "FAIL rewrote an existing context: $out2"; fail=1; }
rm -f "$F"; rm -rf -- "${T:?}"
exit $fail
