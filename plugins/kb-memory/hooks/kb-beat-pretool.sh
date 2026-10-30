#!/usr/bin/env bash
# kb-beat-pretool.sh — a deduped tool-START beat, registered under Claude's
# PreToolUse (OL IDEA4). It lets a quiet live session say what it last
# started: the Now band and `kb sessions status` print `last: Bash · 38m`
# beside `stalled?`, so a 45-minute `cargo build --release` reads as work
# and not as a corpse. The daemon keeps the NAME and the instant, in memory
# only; started is not running, and the label says so.
#
# One beat per change of tool name, or per KB_BEAT_TOOLS_MIN_INTERVAL_SECS
# (default 180) while the same tool repeats. A marker file holds the last name
# sent; its mtime is the interval clock. Same fail-open contract as
# kb-beat-throttle.sh: every failure path is "send nothing, exit 0". A
# PreToolUse hook must NEVER block or deny a tool, so this script never exits
# 2 and never prints to stdout (apart from kb-beat.sh's own DRYRUN body).
#
# Only the tool NAME is read from the payload (`tool_name`), never
# `tool_input`. A payload with a non-empty `agent_id` is a subagent's tool
# call: it never names the parent's last tool, so it sends nothing and the
# parent's own `Agent`/`Task` start stays the evidence.
#
# Kill switches: KB_BEAT=0 (every beat), KB_BEAT_HEARTBEAT=0 (a tool-start beat
# is a mid-turn beat, so silencing the heartbeat silences this too) and
# KB_BEAT_TOOLS=0 (only this).
#
# Usage: kb-beat-pretool.sh <harness>   (stdin = the PreToolUse payload,
# forwarded to `kb-beat.sh <harness> tool start` unmodified when sent)
set -u

case "${KB_BEAT:-}" in
  0 | false | FALSE | no | NO) exit 0 ;;
esac
case "${KB_BEAT_HEARTBEAT:-}" in
  0 | false | FALSE | no | NO) exit 0 ;;
esac
case "${KB_BEAT_TOOLS:-}" in
  0 | false | FALSE | no | NO) exit 0 ;;
esac
[ -n "${KB_SESSIONS_DIR:-}" ] || exit 0
command -v jq >/dev/null 2>&1 || exit 0

MIN_INTERVAL="${KB_BEAT_TOOLS_MIN_INTERVAL_SECS:-180}"
case "$MIN_INTERVAL" in '' | *[!0-9]*) MIN_INTERVAL=180 ;; esac

HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || exit 0
. "$HOOK_DIR/kb-hook-lib.sh" 2>/dev/null || exit 0
BEAT_SH="$HOOK_DIR/kb-beat.sh"

harness="${1:-claude}"

input="$(cat 2>/dev/null)" || input=""
[ -n "$input" ] || exit 0

# ONE jq call. The separator is the ASCII unit separator, NOT a tab: tab is IFS
# whitespace, so `read` would collapse an empty tool_name into the next field
# and a subagent payload could be mistaken for a main-thread one.
fields="$(printf '%s' "$input" | jq -r '[.session_id, .tool_name, .agent_id] | map(if type == "string" then . else "" end) | join("\u001f")' 2>/dev/null)" || exit 0
sid=""
tool=""
agent=""
IFS=$'\x1f' read -r sid tool agent <<<"$fields" || true
[ -n "$sid" ] || exit 0
[ -n "$tool" ] || exit 0
[ -z "$agent" ] || exit 0

marker_dir="${XDG_CACHE_HOME:-$HOME/.cache}/kb"
mkdir -p "$marker_dir" 2>/dev/null || exit 0
marker="$marker_dir/beat-tool-$(hook_sid_key "$sid")"

send=1
if [ -f "$marker" ]; then
  last="$(cat "$marker" 2>/dev/null)" || last=""
  if [ "$last" = "$tool" ]; then
    now="$(date -u +%s)" || exit 0
    mtime="$(stat -c %Y "$marker" 2>/dev/null || stat -f %m "$marker" 2>/dev/null || echo 0)"
    case "$mtime" in '' | *[!0-9]*) mtime=0 ;; esac
    [ $((now - mtime)) -ge "$MIN_INTERVAL" ] || send=0
  fi
fi
[ "$send" -eq 1 ] || exit 0

printf '%s' "$input" | "$BEAT_SH" "$harness" tool start
printf '%s' "$tool" >"$marker" 2>/dev/null || true
exit 0
