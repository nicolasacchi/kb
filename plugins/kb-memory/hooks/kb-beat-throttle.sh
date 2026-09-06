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
# Usage: kb-beat-throttle.sh <harness>   (stdin = the PostToolUse payload,
# forwarded to `kb-beat.sh <harness> tool` unmodified when not throttled)
set -u

case "${KB_BEAT:-}" in
  0 | false | FALSE | no | NO) exit 0 ;;
esac
case "${KB_BEAT_HEARTBEAT:-}" in
  0 | false | FALSE | no | NO) exit 0 ;;
esac
[ -n "${KB_SESSIONS_DIR:-}" ] || exit 0
command -v jq >/dev/null 2>&1 || exit 0

MIN_INTERVAL="${KB_BEAT_HEARTBEAT_MIN_INTERVAL_SECS:-180}"

HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BEAT_SH="$HOOK_DIR/kb-beat.sh"

harness="${1:-claude}"

input="$(cat 2>/dev/null)" || input=""
[ -n "$input" ] || exit 0

sid="$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null)"
[ -n "$sid" ] || {
  # No session id to throttle against — let kb-beat.sh make its own
  # (identical) determination rather than silently drop the beat.
  printf '%s' "$input" | "$BEAT_SH" "$harness" tool
  exit 0
}

marker_dir="${XDG_CACHE_HOME:-$HOME/.cache}/kb"
mkdir -p "$marker_dir" 2>/dev/null || exit 0
safe_sid="$(printf '%s' "$sid" | tr -c 'a-zA-Z0-9' '-' | cut -c1-80)"
marker="$marker_dir/beat-heartbeat-$safe_sid"

now="$(date -u +%s)" || exit 0
if [ -f "$marker" ]; then
  mtime="$(stat -c %Y "$marker" 2>/dev/null || stat -f %m "$marker" 2>/dev/null || echo 0)"
  case "$mtime" in '' | *[!0-9]*) mtime=0 ;; esac
  age=$((now - mtime))
  [ "$age" -ge "$MIN_INTERVAL" ] || exit 0
fi

printf '%s' "$input" | "$BEAT_SH" "$harness" tool
touch "$marker" 2>/dev/null || true
exit 0
