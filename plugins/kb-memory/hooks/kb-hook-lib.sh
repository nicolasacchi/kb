#!/usr/bin/env bash
# kb-hook-lib.sh — helpers every kb-memory hook shares (v0.44 F6, closing the
# H1 carry-over: five divergent copies of post_distill_ask and per-call caps
# that did not add up against the hooks.json timeouts).
#
# SOURCED, never executed. Every consumer sources it fail-open:
#
#   . "$(dirname "$0")/kb-hook-lib.sh" 2>/dev/null || { <inline no-op stubs>; }
#
# so a standalone copy of one hook (README "Mode 1") that lacks this file
# degrades to unbounded calls / no distill ask instead of breaking the turn.
#
# One deadline model, two knobs:
#   KB_HOOK_BUDGET_SECS  total wall budget for the hook (default 13, which is
#                        below the 15s UserPromptSubmit/SessionStart timeouts
#                        in hooks.json — test-hook-deadlines.sh pins that).
#   run_to <cap> cmd...  runs cmd under min(cap, what is left of the budget);
#                        a call with nothing left is skipped (rc 124 = a miss,
#                        never a kill).
# The clock is milliseconds when the shell can supply them, so a hook that
# starts at x.9s is not credited a whole phantom second.

# Milliseconds since the epoch. EPOCHREALTIME (bash 5) is the cheap path; GNU
# date +%s%N next; whole seconds as the last resort.
hook_now_ms() {
  if [ -n "${EPOCHREALTIME:-}" ]; then
    local s="${EPOCHREALTIME%[.,]*}" f="${EPOCHREALTIME#*[.,]}"
    f="${f}000"
    printf '%s' "$((10#$s * 1000 + 10#${f:0:3}))"
    return 0
  fi
  local n
  n="$(date +%s%N 2>/dev/null)"
  case "$n" in
    '' | *[!0-9]*) printf '%s' "$(($(date +%s 2>/dev/null || echo 0) * 1000))" ;;
    *) printf '%s' "$((n / 1000000))" ;;
  esac
}

# Start the shared deadline. Call once, early.
hook_deadline_init() {
  hook_t0_ms="$(hook_now_ms)"
  hook_budget_ms=$((${KB_HOOK_BUDGET_SECS:-13} * 1000))
}

# Milliseconds left on the shared deadline (never negative).
hook_left_ms() {
  [ -n "${hook_t0_ms:-}" ] || hook_deadline_init
  local left=$((hook_budget_ms - ($(hook_now_ms) - hook_t0_ms)))
  [ "$left" -gt 0 ] || left=0
  printf '%s' "$left"
}

# Fractional-seconds support in timeout(1), probed once.
hook_timeout_frac=""
hook_fmt_secs() { # <ms> -> seconds string timeout(1) accepts
  local ms="$1"
  if [ -z "$hook_timeout_frac" ]; then
    if timeout 0.01 true >/dev/null 2>&1; then hook_timeout_frac=1; else hook_timeout_frac=0; fi
  fi
  if [ "$hook_timeout_frac" = 1 ]; then
    printf '%d.%03d' "$((ms / 1000))" "$((ms % 1000))"
  else
    printf '%d' "$(((ms + 999) / 1000))"
  fi
}

run_to() {
  local cap="$1" left cap_ms
  shift
  left="$(hook_left_ms)"
  [ "$left" -gt 0 ] || return 124
  cap_ms=$((cap * 1000))
  [ "$left" -lt "$cap_ms" ] && cap_ms="$left"
  if command -v timeout >/dev/null 2>&1; then
    timeout "$(hook_fmt_secs "$cap_ms")" "$@"
  else
    "$@"
  fi
}

# Post the "distill this session?" slate ask. Call only after the
# commit-without-remember check. Slate stdout is discarded so it cannot
# corrupt the caller's own output. Never blocks (4s cap, loopback must not
# ride HTTP(S)_PROXY — same reason as kb-wake-kimi.sh).
post_distill_ask() {
  local sid="$1" harness="$2" cwd="${3:-}"
  command -v kb >/dev/null 2>&1 || return 0
  local args=(
    slate ask "Distill session ${sid} (${harness})?"
    --harness "$harness"
    --session-id "$sid"
    --ref "session:${sid}"
  )
  [ -n "$cwd" ] && args+=(--cwd "$cwd")
  (
    export NO_PROXY="127.0.0.1,localhost${NO_PROXY:+,$NO_PROXY}"
    export no_proxy="127.0.0.1,localhost${no_proxy:+,$no_proxy}"
    unset HTTP_PROXY HTTPS_PROXY http_proxy https_proxy ALL_PROXY all_proxy
    if command -v timeout >/dev/null 2>&1; then
      timeout 4 kb "${args[@]}" >/dev/null 2>&1 || true
    else
      kb "${args[@]}" >/dev/null 2>&1 || true
    fi
  )
}
