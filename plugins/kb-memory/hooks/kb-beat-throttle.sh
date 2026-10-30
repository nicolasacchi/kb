#!/usr/bin/env bash
# kb-beat-throttle.sh — a min-interval gate in front of kb-beat.sh's
# `tool` event, registered under PostToolUse. Design §11 names this
# exact mitigation: "Beat volume. Per-tool beats would be hundreds per
# session. Beat on turn boundaries and lifecycle only, with a throttled
# mid-turn heartbeat reusing kb-capture-throttle.sh's pattern."
#
# Why a heartbeat at all, rather than omitting PostToolUse entirely: the
# design's own worked example (§3) is a 20-minute `cargo build` inside
# ONE turn — no Stop fires until it's done, so without a mid-turn beat
# the daemon's only signal is the `prompt` beat that opened the turn.
# `lease_secs` would then have to be long enough to cover every plausible
# single tool call, which weakens the `stalled?` (45min-silent) signal
# design §3 defines as the honest "still agent's turn, silence is not
# evidence otherwise" label. A periodic heartbeat keeps the lease fresh
# without per-tool volume.
#
# Unlike kb-capture-throttle.sh (which stats the session's own capture
# HTML file), there is no artifact to check the mtime of here, so the
# gate is a per-session marker file's mtime instead. Same fail-open
# contract as kb-beat.sh: every failure path degrades to "skip silently,
# exit 0" — this script only decides WHETHER to invoke kb-beat.sh, then
# hands it the identical stdin payload unmodified (kb-beat.sh owns all
# field extraction / body construction / posting).
#
# Default ON (unlike kb-capture-throttle.sh's default-off
# KB_CAPTURE_LIVE gate) — a beat is a few-hundred-byte fire-and-forget
# POST, not a multi-MB transcript re-serialize + reindex, so the cost
# profile that justified capture's opt-in default doesn't apply here.
# Two independent kill switches: KB_BEAT=0 (kills every beat, matching
# kb-beat.sh) and KB_BEAT_HEARTBEAT=0 (kills only this mid-turn
# heartbeat, keeping lifecycle beats).
#
# Blocked-on-you resolution (claude): the harness has no "permission granted"
# hook, so the FIRST PostToolUse from the same agent that raised a blocked beat
# (kb-beat.sh leaves beat-blocked-<sid> holding that agent_id, empty for the
# main thread) emits an `unblocked` lifecycle beat. It bypasses the interval
# gate and the KB_BEAT_HEARTBEAT=0 switch (a lifecycle beat, not a heartbeat),
# once per block.
#
# Usage: kb-beat-throttle.sh <harness>   (stdin = the PostToolUse payload,
# forwarded to `kb-beat.sh <harness> tool` unmodified when not throttled)
set -u

case "${KB_BEAT:-}" in
  0 | false | FALSE | no | NO) exit 0 ;;
esac
[ -n "${KB_SESSIONS_DIR:-}" ] || exit 0
command -v jq >/dev/null 2>&1 || exit 0

MIN_INTERVAL="${KB_BEAT_HEARTBEAT_MIN_INTERVAL_SECS:-180}"

HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HOOK_DIR/kb-hook-lib.sh" 2>/dev/null || {
  # No shared lib => no unified session key: fail open (no heartbeat).
  exit 0
}
BEAT_SH="$HOOK_DIR/kb-beat.sh"

harness="${1:-claude}"

input="$(cat 2>/dev/null)" || input=""
[ -n "$input" ] || exit 0

# One jq for both fields: line 1 = session id, line 2 = tool name.
{ IFS= read -r sid; IFS= read -r tool_name; } < <(
  printf '%s' "$input" | jq -r '(.session_id // ""), (.tool_name // "")' 2>/dev/null
) || true

[ -n "$sid" ] || {
  # No session id to throttle against — let kb-beat.sh make its own
  # (identical) determination rather than silently drop the beat.
  printf '%s' "$input" | "$BEAT_SH" "$harness" tool
  exit 0
}

marker_dir="${XDG_CACHE_HOME:-$HOME/.cache}/kb"
mkdir -p "$marker_dir" 2>/dev/null || exit 0
safe_sid="$(hook_sid_key "$sid")"
marker="$marker_dir/beat-heartbeat-$safe_sid"

# Upgrade compatibility: a heartbeat marker written under the pre-v0.45 lossy
# name still throttles (its mtime is read), so no extra beat fires after the
# upgrade. The new marker is always written under the unified key.
gate="$marker"
if [ ! -f "$marker" ]; then
  legacy="$marker_dir/beat-heartbeat-$(hook_sid_key_lossy "$sid")"
  [ "$legacy" != "$marker" ] && [ -f "$legacy" ] && gate="$legacy"
fi

blocked_marker="$marker_dir/beat-blocked-$safe_sid"
if [ "$harness" = "claude" ] && [ -f "$blocked_marker" ]; then
  marker_agent="$(cat "$blocked_marker" 2>/dev/null)" || marker_agent=""
  this_agent="$(printf '%s' "$input" | jq -r '.agent_id // empty' 2>/dev/null)"
  if [ "$marker_agent" = "$this_agent" ]; then
    rm -f "$blocked_marker" 2>/dev/null
    printf '%s' "$input" | "$BEAT_SH" "$harness" unblocked
    touch "$marker" 2>/dev/null || true
    gate="$marker"
  fi
fi

# OL PRES2 — a Claude write-tool call (Edit/Write/MultiEdit/NotebookEdit)
# is a file-presence beat, not a heartbeat: it bypasses the min-interval
# gate (and KB_BEAT_HEARTBEAT=0; it runs AFTER the blocked-on-you resolution,
# so an edit by the blocked agent still emits `unblocked` first) and goes straight to kb-beat.sh, so one
# hook process tree serves each edit instead of a second PostToolUse
# group. KB_BEAT_PATHS=0 removes the paths from the beat, so there is
# nothing to report and such a call stays an ordinary throttled tool call.
if [ "$harness" = "claude" ] && [ "${KB_BEAT_PATHS:-}" != "0" ]; then
  case "$tool_name" in
    Edit | Write | MultiEdit | NotebookEdit)
      printf '%s' "$input" | "$BEAT_SH" "$harness" tool
      touch "$marker" 2>/dev/null || true
      exit 0
      ;;
  esac
fi

case "${KB_BEAT_HEARTBEAT:-}" in
  0 | false | FALSE | no | NO) exit 0 ;;
esac

now="$(date -u +%s)" || exit 0
if [ -f "$gate" ]; then
  mtime="$(stat -c %Y "$gate" 2>/dev/null || stat -f %m "$gate" 2>/dev/null || echo 0)"
  case "$mtime" in '' | *[!0-9]*) mtime=0 ;; esac
  age=$((now - mtime))
  [ "$age" -ge "$MIN_INTERVAL" ] || exit 0
fi

printf '%s' "$input" | "$BEAT_SH" "$harness" tool
touch "$marker" 2>/dev/null || true
exit 0
