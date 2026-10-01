#!/usr/bin/env bash
# Static gate for the dashboard SPA (crates/amux-dashboard/static/*.js).
#
# Exists because two silent-dead-handler bugs shipped in one week (schedule
# delete, worker delete 2026-08-09): an undefined name inside an async handler
# makes the click do NOTHING — no dialog, no request, no error. no-undef kills
# that class statically; the runtime rejection-toast in app.js is only the floor.
#
# Usage: scripts/spa-lint.sh          (from anywhere; CI and agents run this)
# Exit code is eslint's own — 0 only when there are no errors. Warnings
# (no-unused-vars) do not fail the gate; errors (no-undef etc.) do.
set -euo pipefail
cd "$(dirname "$0")/.."

# Regenerate the globals allowlist from the actual code every run, so the
# lint can never pass against a stale picture of the SPA's global surface.
node scripts/gen-spa-globals.mjs
node scripts/build-state.mjs --check
# Every async button press goes through the shared press-feedback layer, and
# docs/ux/button-feedback-audit.md is current (AMUX-5417). Fails on a new
# write started by pointerdown/touch, or an async control the layer cannot see.
node scripts/button-audit.mjs --check

exec ./node_modules/.bin/eslint 'crates/amux-dashboard/static/*.js'
