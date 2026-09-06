#!/usr/bin/env bash
# test-capture-throttle.sh — self-contained test matrix for
# kb-capture-throttle.sh (LF-3a/D2). Runs the REAL script copied beside a
# MOCK kb-capture.sh (so `HOOK_DIR="$(dirname "${BASH_SOURCE[0]}")"`
# resolves to the mock, never the real capture pipeline) in a scratch
# tempdir. No network, no daemon.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-capture-throttle.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
THROTTLE_SRC="$HOOKS_DIR/kb-capture-throttle.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-throttle-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

# One isolated rig per test: a fresh hooks dir (throttle copy + mock
# kb-capture.sh that just appends its stdin's session_id to a call log) +
# a fresh KB_SESSIONS_DIR.
setup_rig() {
  local rig="$1"
  mkdir -p "$rig/hooks" "$rig/sessions"
  cp "$THROTTLE_SRC" "$rig/hooks/kb-capture-throttle.sh"
  chmod +x "$rig/hooks/kb-capture-throttle.sh"
  cat >"$rig/hooks/kb-capture.sh" <<'EOF'
#!/usr/bin/env bash
input="$(cat)"
sid="$(printf '%s' "$input" | jq -r '.session_id // "unknown"')"
echo "$sid" >> "$KB_CAPTURE_CALL_LOG"
touch "$KB_SESSIONS_DIR/session-$(date -u +%Y%m%dT%H%M%SZ)-$sid.html.marker" 2>/dev/null || true
EOF
  chmod +x "$rig/hooks/kb-capture.sh"
}

run_throttle() {
  local rig="$1" payload="$2"
  printf '%s' "$payload" | "$rig/hooks/kb-capture-throttle.sh"
}

# --- 1. default off (KB_CAPTURE_LIVE unset) ---------------------------------
rig="$TMPROOT/t1"
setup_rig "$rig"
export KB_SESSIONS_DIR="$rig/sessions"
export KB_CAPTURE_CALL_LOG="$rig/calls.log"
unset KB_CAPTURE_LIVE
: >"$KB_CAPTURE_CALL_LOG"
run_throttle "$rig" '{"session_id":"sid-1","transcript_path":"/tmp/x.jsonl"}'
if [ ! -s "$KB_CAPTURE_CALL_LOG" ]; then
  ok "KB_CAPTURE_LIVE unset -> never invokes kb-capture.sh"
else
  bad "KB_CAPTURE_LIVE unset -> never invokes kb-capture.sh (log: $(cat "$KB_CAPTURE_CALL_LOG"))"
fi

# --- 2. first capture for a sid fires immediately (nothing to throttle) ----
rig="$TMPROOT/t2"
setup_rig "$rig"
export KB_SESSIONS_DIR="$rig/sessions"
export KB_CAPTURE_CALL_LOG="$rig/calls.log"
export KB_CAPTURE_LIVE=1
: >"$KB_CAPTURE_CALL_LOG"
run_throttle "$rig" '{"session_id":"sid-2","transcript_path":"/tmp/x.jsonl"}'
if [ "$(cat "$KB_CAPTURE_CALL_LOG")" = "sid-2" ]; then
  ok "no existing capture -> fires immediately"
else
  bad "no existing capture -> fires immediately (log: $(cat "$KB_CAPTURE_CALL_LOG"))"
fi

# --- 3. a RECENT existing capture is throttled (skipped) --------------------
rig="$TMPROOT/t3"
setup_rig "$rig"
export KB_SESSIONS_DIR="$rig/sessions"
export KB_CAPTURE_CALL_LOG="$rig/calls.log"
export KB_CAPTURE_LIVE=1
export KB_CAPTURE_MIN_INTERVAL_SECS=240
touch "$rig/sessions/session-20260101T000000Z-sid-3.html"
: >"$KB_CAPTURE_CALL_LOG"
run_throttle "$rig" '{"session_id":"sid-3","transcript_path":"/tmp/x.jsonl"}'
if [ ! -s "$KB_CAPTURE_CALL_LOG" ]; then
  ok "recent existing capture -> throttled (skipped)"
else
  bad "recent existing capture -> throttled (skipped) (log: $(cat "$KB_CAPTURE_CALL_LOG"))"
fi

# --- 4. an OLD existing capture (past the interval) fires again -------------
rig="$TMPROOT/t4"
setup_rig "$rig"
export KB_SESSIONS_DIR="$rig/sessions"
export KB_CAPTURE_CALL_LOG="$rig/calls.log"
export KB_CAPTURE_LIVE=1
export KB_CAPTURE_MIN_INTERVAL_SECS=5
touch "$rig/sessions/session-20260101T000000Z-sid-4.html"
old_ts="$(( $(date -u +%s) - 3600 ))"
touch -d "@$old_ts" "$rig/sessions/session-20260101T000000Z-sid-4.html" 2>/dev/null \
  || touch -t "$(date -u -r "$old_ts" +%Y%m%d%H%M.%S 2>/dev/null)" "$rig/sessions/session-20260101T000000Z-sid-4.html" 2>/dev/null \
  || true
: >"$KB_CAPTURE_CALL_LOG"
run_throttle "$rig" '{"session_id":"sid-4","transcript_path":"/tmp/x.jsonl"}'
if [ "$(cat "$KB_CAPTURE_CALL_LOG" 2>/dev/null)" = "sid-4" ]; then
  ok "old existing capture (past interval) -> fires again"
else
  bad "old existing capture (past interval) -> fires again (log: $(cat "$KB_CAPTURE_CALL_LOG" 2>/dev/null))"
fi

# --- 5. KB_SESSIONS_DIR unset -> no-op regardless of KB_CAPTURE_LIVE --------
rig="$TMPROOT/t5"
setup_rig "$rig"
unset KB_SESSIONS_DIR
export KB_CAPTURE_CALL_LOG="$rig/calls.log"
export KB_CAPTURE_LIVE=1
: >"$KB_CAPTURE_CALL_LOG"
printf '%s' '{"session_id":"sid-5"}' | "$rig/hooks/kb-capture-throttle.sh"
if [ ! -s "$KB_CAPTURE_CALL_LOG" ]; then
  ok "KB_SESSIONS_DIR unset -> no-op"
else
  bad "KB_SESSIONS_DIR unset -> no-op (log: $(cat "$KB_CAPTURE_CALL_LOG"))"
fi
export KB_SESSIONS_DIR="$rig/sessions"

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
