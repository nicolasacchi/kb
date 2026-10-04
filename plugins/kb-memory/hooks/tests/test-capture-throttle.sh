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

# --- 6. v0.45 N4: distinct non-UUID ids get distinct throttle slots ---------
# The mock capture writes `session-<ts>-<hook_sid_key>.html` (the real name),
# so a shared (lossy) slot would let the second id's capture be throttled by
# the first's file.
setup_rig6() {
  local rig="$1"
  setup_rig "$rig"
  cp "$HOOKS_DIR/kb-hook-lib.sh" "$rig/hooks/kb-hook-lib.sh"
  cat >"$rig/hooks/kb-capture.sh" <<'EOF'
#!/usr/bin/env bash
. "$(dirname "$0")/kb-hook-lib.sh"
input="$(cat)"
sid="$(printf '%s' "$input" | jq -r '.session_id // "unknown"')"
echo "$sid" >> "$KB_CAPTURE_CALL_LOG"
printf '{"sessionId":%s}\n' "$(printf '%s' "$sid" | jq -Rs .)" \
  >"$KB_SESSIONS_DIR/session-$(date -u +%Y%m%dT%H%M%SZ)-$(hook_sid_key "$sid").html"
EOF
  chmod +x "$rig/hooks/kb-capture.sh"
}
rig="$TMPROOT/t6"
setup_rig6 "$rig"
export KB_SESSIONS_DIR="$rig/sessions"
export KB_CAPTURE_CALL_LOG="$rig/calls.log"
export KB_CAPTURE_LIVE=1
export KB_CAPTURE_MIN_INTERVAL_SECS=240
: >"$KB_CAPTURE_CALL_LOG"
run_throttle "$rig" '{"session_id":"a_b","transcript_path":"/tmp/x.jsonl"}'
run_throttle "$rig" '{"session_id":"a-b","transcript_path":"/tmp/x.jsonl"}'
if [ "$(tr '\n' ' ' <"$KB_CAPTURE_CALL_LOG")" = "a_b a-b " ]; then
  ok "throttle_distinct_ids_a_b_vs_a_dash_b_do_not_share_a_throttle_slot"
else
  bad "throttle_distinct_ids_a_b_vs_a_dash_b_do_not_share_a_throttle_slot (log: $(tr '\n' ' ' <"$KB_CAPTURE_CALL_LOG"))"
fi
# ...and each id IS throttled by its OWN file.
run_throttle "$rig" '{"session_id":"a_b","transcript_path":"/tmp/x.jsonl"}'
run_throttle "$rig" '{"session_id":"a-b","transcript_path":"/tmp/x.jsonl"}'
if [ "$(wc -l <"$KB_CAPTURE_CALL_LOG" | tr -d ' ')" = 2 ]; then
  ok "each distinct id is throttled by its own capture file"
else
  bad "each distinct id is throttled by its own capture file (log: $(tr '\n' ' ' <"$KB_CAPTURE_CALL_LOG"))"
fi

# --- 7. legacy lossy-named capture still throttles its own id ----------------
rig="$TMPROOT/t7"
setup_rig6 "$rig"
export KB_SESSIONS_DIR="$rig/sessions"
export KB_CAPTURE_CALL_LOG="$rig/calls.log"
: >"$KB_CAPTURE_CALL_LOG"
printf '<pre>{"sessionId":"ses_legacy_1","type":"user"}\n' \
  >"$rig/sessions/session-20260101T000000Z-ses-legacy-1.html"
run_throttle "$rig" '{"session_id":"ses_legacy_1","transcript_path":"/tmp/x.jsonl"}'
if [ ! -s "$KB_CAPTURE_CALL_LOG" ]; then
  ok "throttle_finds_legacy_lossy_named_capture_for_matching_id"
else
  bad "throttle_finds_legacy_lossy_named_capture_for_matching_id (log: $(cat "$KB_CAPTURE_CALL_LOG"))"
fi
# trailing-dash legacy variant too
rig="$TMPROOT/t7b"
setup_rig6 "$rig"
export KB_SESSIONS_DIR="$rig/sessions"
export KB_CAPTURE_CALL_LOG="$rig/calls.log"
: >"$KB_CAPTURE_CALL_LOG"
printf '<pre>{"sessionId":"ses_legacy_1","type":"user"}\n' \
  >"$rig/sessions/session-20260101T000000Z-ses-legacy-1-.html"
run_throttle "$rig" '{"session_id":"ses_legacy_1","transcript_path":"/tmp/x.jsonl"}'
if [ ! -s "$KB_CAPTURE_CALL_LOG" ]; then
  ok "legacy trailing-dash capture also throttles its id"
else
  bad "legacy trailing-dash capture also throttles its id (log: $(cat "$KB_CAPTURE_CALL_LOG"))"
fi

# --- 8. a legacy file belonging to a COLLIDING id is not adopted -------------
rig="$TMPROOT/t8"
setup_rig6 "$rig"
export KB_SESSIONS_DIR="$rig/sessions"
export KB_CAPTURE_CALL_LOG="$rig/calls.log"
: >"$KB_CAPTURE_CALL_LOG"
# The lossy name "ses-legacy-1" holds a DIFFERENT session ("ses-legacy-1"
# itself); a capture for "ses_legacy_1" must fire, not be throttled by it.
printf '<pre>{"sessionId":"ses-legacy-1","type":"user"}\n' \
  >"$rig/sessions/session-20260101T000000Z-ses-legacy-1.html"
run_throttle "$rig" '{"session_id":"ses_legacy_1","transcript_path":"/tmp/x.jsonl"}'
if [ "$(cat "$KB_CAPTURE_CALL_LOG")" = "ses_legacy_1" ]; then
  ok "throttle_ignores_legacy_file_of_a_colliding_id"
else
  bad "throttle_ignores_legacy_file_of_a_colliding_id (log: $(cat "$KB_CAPTURE_CALL_LOG"))"
fi
# ...while the plain id that really owns that file IS throttled by it.
: >"$KB_CAPTURE_CALL_LOG"
run_throttle "$rig" '{"session_id":"ses-legacy-1","transcript_path":"/tmp/x.jsonl"}'
if [ ! -s "$KB_CAPTURE_CALL_LOG" ]; then
  ok "the plain id that owns the file is throttled by it"
else
  bad "the plain id that owns the file is throttled by it (log: $(cat "$KB_CAPTURE_CALL_LOG"))"
fi

# --- 9. plain UUID behaviour unchanged (throttled by its own clean file) -----
rig="$TMPROOT/t9"
setup_rig6 "$rig"
export KB_SESSIONS_DIR="$rig/sessions"
export KB_CAPTURE_CALL_LOG="$rig/calls.log"
: >"$KB_CAPTURE_CALL_LOG"
U="0b7e5a1c-3f4d-4e8a-9c21-5d6f7a8b9c0d"
printf '{"sessionId":"%s"}\n' "$U" >"$rig/sessions/session-20260101T000000Z-$U.html"
run_throttle "$rig" "{\"session_id\":\"$U\",\"transcript_path\":\"/tmp/x.jsonl\"}"
if [ ! -s "$KB_CAPTURE_CALL_LOG" ]; then
  ok "plain UUID: recent clean capture still throttles"
else
  bad "plain UUID: recent clean capture still throttles (log: $(cat "$KB_CAPTURE_CALL_LOG"))"
fi

# --- 10. REAL kb capture, then the throttle on the file it produced ----------
if [ -n "${KB_BIN_DIR:-}" ] && [ -x "$KB_BIN_DIR/kb" ]; then
  rig="$TMPROOT/t10"
  mkdir -p "$rig/sessions" "$rig/hooks"
  cp "$HOOKS_DIR/kb-hook-lib.sh" "$HOOKS_DIR/kb-capture.sh" "$THROTTLE_SRC" "$rig/hooks/"
  export KB_SESSIONS_DIR="$rig/sessions"
  export KB_CAPTURE_SPOOL="$rig/spool"
  export KB_HOOK_REAL_PATH="$KB_BIN_DIR:$PATH"
  for sid in ses_real_1 ses-real-1; do
    printf '{"sessionId":"%s","type":"user","timestamp":"2026-03-01T09:00:00.000Z","message":{"role":"user","content":"hi"},"promptSource":"typed"}\n' \
      "$sid" >"$rig/$sid.jsonl"
    printf '{"session_id":"%s","transcript_path":"%s"}' "$sid" "$rig/$sid.jsonl" \
      | PATH="$KB_HOOK_REAL_PATH" KB_CAPTURE_LIVE=1 "$rig/hooks/kb-capture-throttle.sh"
  done
  n="$(find "$rig/sessions" -name 'session-*.html' | wc -l | tr -d ' ')"
  if [ "$n" = 2 ]; then
    ok "real kb: a_b-style and a-b-style ids produce two captures through the throttle"
  else
    bad "real kb: two distinct ids produced $n capture file(s)"
  fi
  # Age each file to 10s old: still inside the 240s interval -> a repeat must NOT rewrite.
  age="$(( $(date -u +%s) - 10 ))"
  for f in "$rig"/sessions/session-*.html; do touch -d "@$age" "$f"; done
  printf '{"session_id":"ses_real_1","transcript_path":"%s"}' "$rig/ses_real_1.jsonl" \
    | PATH="$KB_HOOK_REAL_PATH" KB_CAPTURE_LIVE=1 KB_CAPTURE_MIN_INTERVAL_SECS=240 "$rig/hooks/kb-capture-throttle.sh"
  moved=0
  for f in "$rig"/sessions/session-*.html; do
    [ "$(stat -c %Y "$f")" = "$age" ] || moved=$((moved + 1))
  done
  if [ "$moved" = 0 ]; then
    ok "real kb: the throttle finds the file the Rust engine named (repeat within interval skipped)"
  else
    bad "real kb: throttle missed the Rust-named file ($moved file(s) rewritten)"
  fi
else
  bad "real-kb throttle/capture agreement: a real kb binary is required (KB_BIN_DIR)"
fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
