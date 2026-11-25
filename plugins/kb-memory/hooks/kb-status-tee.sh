#!/usr/bin/env bash
# kb-status-tee.sh -- a pass-through wrapper for Claude Code's statusLine
# command that also feeds the daemon's typed `status` beat.
#
#   kb-status-tee.sh -- <your existing statusLine command and args>
#
# What it does, in this order:
#   1. reads the statusLine JSON from stdin ONCE (byte-exact);
#   2. runs the wrapped command with that same stdin, its stdout and stderr
#      going straight through and its exit code returned unchanged -- the
#      visible status line is whatever the wrapped command prints, nothing else;
#   3. then forks ONE detached background job that derives the status object
#      (model, context %, cost, permission mode, git branch) and hands it to
#      `kb-beat.sh <harness> status`, which owns the floor/dedupe marker and the
#      curl. Nothing in step 3 can delay, alter or fail the visible line.
#
# No wrapped command: prints nothing, exits 0. KB_BEAT=0, KB_BEAT_STATUS=0, no
# KB_SESSIONS_DIR, no jq/curl, or a daemon that is down: a silent pure
# pass-through (the beat is fire-and-forget).
#
# The fields are read tolerantly from the statusLine JSON (names vary by Claude
# Code version); an absent field is omitted, never null- or zero-filled.
#
# Env: KB_STATUS_MIN_INTERVAL_SECS (5), KB_STATUS_REFRESH_SECS (120),
# KB_STATUS_NOW (epoch override, tests) -- read by kb-beat.sh.
set -u

[ "${1:-}" = "--" ] && shift

# Byte-exact stdin (command substitution would strip trailing newlines).
input="$(cat 2>/dev/null; printf x)"
input="${input%x}"

rc=0
if [ "$#" -gt 0 ]; then
  printf '%s' "$input" | "$@"
  rc=$?
fi

# --- status feed: cheap gates only in the foreground. ----------------------
case "${KB_BEAT:-}" in 0 | false | FALSE | no | NO) exit "$rc" ;; esac
[ "${KB_BEAT_STATUS:-}" != "0" ] || exit "$rc"
[ -n "${KB_SESSIONS_DIR:-}" ] || exit "$rc"
command -v jq >/dev/null 2>&1 || exit "$rc"
command -v curl >/dev/null 2>&1 || exit "$rc"
[ -n "$input" ] || exit "$rc"

_tee_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" 2>/dev/null && pwd)" || _tee_dir=""
[ -n "$_tee_dir" ] && [ -x "$_tee_dir/kb-beat.sh" ] || exit "$rc"

(
  # Fail open on every step: this job may never touch the caller's stdio.
  if [ -r "$_tee_dir/kb-hook-lib.sh" ]; then . "$_tee_dir/kb-hook-lib.sh" 2>/dev/null || true; fi
  derived="$(printf '%s' "$input" | jq -c '
    def num: if type == "number" then . else null end;
    def str: if type == "string" and . != "" then . else null end;
    def get(f): try f catch null;
    { session_id: get(.session_id | str),
      cwd: get((.workspace.current_dir // .cwd) | str),
      status: ({ model: get((.model | if type == "object" then (.display_name // .id) else . end) | str),
                 context_pct: get(.context_window.used_percentage | num),
                 cost_usd: get(.cost.total_cost_usd | num),
                 permission_mode: get(.permission_mode | str) }
               | with_entries(select(.value != null))) }
    | select(.session_id != null)' 2>/dev/null)" || exit 0
  [ -n "$derived" ] || exit 0
  sid="$(printf '%s' "$derived" | jq -r '.session_id' 2>/dev/null)"
  cwd="$(printf '%s' "$derived" | jq -r '.cwd // empty' 2>/dev/null)"

  # Cheap pre-gate: a send inside the floor is held anyway (kb-beat.sh decides
  # for real), so skip the git + jq work while the marker is fresh.
  min="${KB_STATUS_MIN_INTERVAL_SECS:-5}"
  case "$min" in '' | *[!0-9]*) min=5 ;; esac
  if command -v hook_sid_key >/dev/null 2>&1; then
    f="${XDG_CACHE_HOME:-$HOME/.cache}/kb/status-$(hook_sid_key "$sid")"
    last_ts=""
    [ -r "$f" ] && { IFS= read -r last_ts <"$f"; } 2>/dev/null
    now="${KB_STATUS_NOW:-$(date +%s 2>/dev/null)}"
    case "$last_ts:$now" in
      *[!0-9:]* | :* | *:) ;;
      *) [ $((now - last_ts)) -ge "$min" ] || exit 0 ;;
    esac
  fi

  branch=""
  if [ -n "$cwd" ] && command -v git >/dev/null 2>&1; then
    if command -v timeout >/dev/null 2>&1; then
      branch="$(timeout 1 git -C "$cwd" branch --show-current 2>/dev/null)" || branch=""
    else
      branch="$(git -C "$cwd" branch --show-current 2>/dev/null)" || branch=""
    fi
  fi
  payload="$(printf '%s' "$derived" | jq -c --arg br "$branch" \
    '.status += (if $br != "" then {branch: $br} else {} end) | select((.status | length) > 0)' 2>/dev/null)" || exit 0
  [ -n "$payload" ] || exit 0
  printf '%s' "$payload" | "$_tee_dir/kb-beat.sh" claude status
) </dev/null >/dev/null 2>&1 &
disown 2>/dev/null || true

exit "$rc"
