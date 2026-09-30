#!/usr/bin/env bash
# git credential helper for github.com: git operations authenticate with the
# owner's long-lived `gh` login, never with a short-lived App token that
# happens to be exported in the calling shell.
#
# Why (2026-09-30, gs12-cicd then gs12-spend): lanes run
# `eval "$(~/.amux/github-app/get-token.sh)"`, exporting GH_TOKEN (a GitHub App
# installation token, at most an hour of life). `gh auth git-credential`
# answers git with GH_TOKEN, and git fetches its credential ONCE, when the push
# connects, before the pre-push hook. Mixpeek's hook runs 15 to 60 minutes, so
# pushes that had passed every check failed with HTTP 401 at the send.
#
# The first version of this helper minted a fresh App token per request on the
# assumption that git asks again after a 401. It does not: measured with a
# helper that returned a bad token first, git called get, erase, and failed
# with no second get. So a token fetched at connect has to outlive the whole
# push, and only the owner's login does.
#
# get: `gh auth git-credential get` with GH_TOKEN and GITHUB_TOKEN removed, so
#      gh answers from its stored login. If gh has no stored login, fall back
#      to an App token with at least 55 minutes left.
# store/erase: nothing to keep; the stored login is gh's to manage.
#
# Installed by install.sh as credential.https://github.com.helper when
# ~/.amux/github-app/get-token.sh exists. Each answer logs which source it used
# to ~/.amux/logs/git-credential.log (never the token).
set -u
op="${1:-get}"
input="$(cat)"
host="$(printf '%s\n' "$input" | sed -n 's/^host=//p' | head -1)"
mint="${AMUX_GITHUB_TOKEN_SCRIPT:-$HOME/.amux/github-app/get-token.sh}"
gh_bin="$(command -v gh || echo /usr/local/bin/gh)"
log() { mkdir -p "$HOME/.amux/logs"; printf '%s %s (pid %s)\n' "$(date -u +%FT%TZ)" "$1" "$PPID" >> "$HOME/.amux/logs/git-credential.log"; }

if [ "$host" != "github.com" ]; then
  printf '%s\n' "$input" | exec "$gh_bin" auth git-credential "$op"
fi
case "$op" in
  get)
    out="$(printf '%s\n' "$input" | env -u GH_TOKEN -u GITHUB_TOKEN "$gh_bin" auth git-credential get 2>/dev/null)"
    if printf '%s\n' "$out" | grep -q '^password='; then
      printf '%s\n' "$out"
      log "gh-login answered git"
      exit 0
    fi
    if [ -x "$mint" ]; then
      tok="$("$mint" --raw --min-remaining 3300 2>/dev/null)"
      if [ -n "$tok" ]; then
        printf 'username=x-access-token\npassword=%s\n' "$tok"
        log "no gh login; app-token (>=55 min) answered git"
        exit 0
      fi
    fi
    log "no credential available"
    exit 0
    ;;
  store|erase) exit 0 ;;
esac
