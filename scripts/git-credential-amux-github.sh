#!/usr/bin/env bash
# git credential helper for github.com: a fresh GitHub App token every time
# git asks, when the caller is working under an App token.
#
# Why (2026-09-30, gs12-cicd): lanes run `eval "$(~/.amux/github-app/get-token.sh)"`,
# which exports GH_TOKEN (an App installation token, about an hour of life).
# git's github.com helper, `gh auth git-credential`, answers with GH_TOKEN, and
# git asks for it when the push CONNECTS, before the pre-push hook. Mixpeek's
# pre-push checks run 15 to 60 minutes, so the token expired before the pack
# was sent and two pushes that had passed every check failed with HTTP 401.
#
# git asks again after a 401 (erase, then get), so a helper that mints on every
# `get` turns an expiry into a retry. When GH_TOKEN is not an App token (or is
# unset), this hands the request to `gh auth git-credential` unchanged, so the
# owner's own login keeps working exactly as before.
#
# Installed by install.sh as credential.https://github.com.helper when
# ~/.amux/github-app/get-token.sh exists. Each App mint logs one line to
# ~/.amux/logs/git-credential.log (never the token).
set -u
op="${1:-get}"
input="$(cat)"
host="$(printf '%s\n' "$input" | sed -n 's/^host=//p' | head -1)"
mint="${AMUX_GITHUB_TOKEN_SCRIPT:-$HOME/.amux/github-app/get-token.sh}"
gh_bin="$(command -v gh || echo /usr/local/bin/gh)"

if [ "$host" = "github.com" ] && [ "${GH_TOKEN:-}" != "${GH_TOKEN#ghs_}" ] && [ -x "$mint" ]; then
  case "$op" in
    get)
      tok="$("$mint" --raw --min-remaining 1800 2>/dev/null)"
      if [ -n "$tok" ]; then
        printf 'username=x-access-token\npassword=%s\n' "$tok"
        mkdir -p "$HOME/.amux/logs"
        printf '%s app-token minted for git (pid %s)\n' "$(date -u +%FT%TZ)" "$PPID" >> "$HOME/.amux/logs/git-credential.log"
        exit 0
      fi
      echo "git-credential-amux-github: could not mint an App token; falling back to gh" >&2
      ;;
    store|erase) exit 0 ;;  # nothing to persist: every get mints
  esac
fi
printf '%s\n' "$input" | exec "$gh_bin" auth git-credential "$op"
