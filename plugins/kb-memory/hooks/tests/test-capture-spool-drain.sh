#!/usr/bin/env bash
# test-capture-spool-drain.sh - a spooled capture too big to land inside the
# Stop hook's own replay slice must still drain, WITHOUT the hook waiting for
# it. Uses a fake `kb` whose `--replay-spool` takes longer than the synchronous
# slice (KB_SPOOL_SYNC_SECS=1 here; 10 in production), so only the detached
# background drain can land the item.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-capture-spool-drain.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-spool-drain-test.XXXXXX")"
LIVE_PIDS=""
cleanup() {
  # shellcheck disable=SC2086
  [ -n "$LIVE_PIDS" ] && kill $LIVE_PIDS 2>/dev/null
  rm -rf "$TMPROOT"
}
trap cleanup EXIT
. "$SCRIPT_DIR/lib-sandbox.sh"
kb_test_sandbox "$TMPROOT" || exit 1

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

command -v timeout >/dev/null 2>&1 || { echo "SKIP: no timeout(1)"; echo "passed=0 failed=0 skipped=1"; exit 0; }

export KB_CAPTURE_SPOOL="$TMPROOT/spool"
export KB_SESSIONS_DIR="$TMPROOT/sessions"
export KB_SPOOL_SYNC_SECS=1
mkdir -p "$KB_SESSIONS_DIR"
LOG="$TMPROOT/kb-calls.log"
LANDED="$TMPROOT/landed"

# Fake kb. `capture --transcript` succeeds at once. `capture --replay-spool`
# logs, sleeps $FAKE_REPLAY_SECS (the "large item"), then lands + deletes every
# spool item (as the real replay does).
mkdir -p "$TMPROOT/fakebin"
cat >"$TMPROOT/fakebin/kb" <<'KB'
#!/usr/bin/env bash
case " $* " in
  *" --replay-spool "*)
    echo "replay $$" >>"$FAKE_KB_LOG"
    sleep "${FAKE_REPLAY_SECS:-0}"
    mkdir -p "$FAKE_KB_LANDED"
    for f in "$KB_CAPTURE_SPOOL"/*.jsonl; do
      [ -f "$f" ] || continue
      cp "$f" "$FAKE_KB_LANDED/"
      rm -f "$f" "${f%.jsonl}.meta"
    done
    exit 0 ;;
esac
exit 0
KB
chmod +x "$TMPROOT/fakebin/kb"
export FAKE_KB_LOG="$LOG" FAKE_KB_LANDED="$LANDED"

SID="sess-drain-0001"
TRANSCRIPT="$TMPROOT/t.jsonl"
printf '{"sessionId":"%s","type":"user","message":{"role":"user","content":"hi"}}\n' "$SID" >"$TRANSCRIPT"
PAYLOAD="{\"session_id\":\"$SID\",\"transcript_path\":\"$TRANSCRIPT\",\"cwd\":\"$TMPROOT\"}"

park() { # park a spool item of ANOTHER session (this session's own is dropped on success)
  mkdir -p "$KB_CAPTURE_SPOOL"
  printf 'x\n' >"$KB_CAPTURE_SPOOL/other-sess.jsonl"
  printf 'session_id=other-sess\n' >"$KB_CAPTURE_SPOOL/other-sess.meta"
}
reset() { rm -rf "$KB_CAPTURE_SPOOL" "$LANDED" "$LOG"; }
run_hook() { printf '%s' "$PAYLOAD" | PATH="$TMPROOT/fakebin:$PATH" bash "$HOOKS_DIR/kb-capture.sh" >/dev/null 2>&1; }
wait_for() { # <seconds> <test-cmd...>
  local n="$1"; shift
  local i=0
  while [ "$i" -lt $((n * 10)) ]; do
    "$@" && return 0
    sleep 0.1; i=$((i + 1))
  done
  return 1
}
landed_it() { [ -f "$LANDED/other-sess.jsonl" ]; }
lock_gone() { [ ! -e "$KB_CAPTURE_SPOOL/.drain.lock" ]; }
calls() { [ -f "$LOG" ] && wc -l <"$LOG" | tr -d ' ' || echo 0; }

echo "== a replay slower than the sync slice drains in the background =="
reset; park
export FAKE_REPLAY_SECS=4
t0=$(date +%s)
run_hook
el=$(( $(date +%s) - t0 ))
[ "$el" -le 3 ] && ok "the hook returned at its sync slice, not the 4s replay (${el}s)" || bad "the hook waited out the replay (${el}s)"
[ -f "$KB_CAPTURE_SPOOL/other-sess.jsonl" ] && ok "the item is still spooled when the hook exits" || bad "the item was already gone"
wait_for 15 landed_it && ok "the detached drain lands the item" || bad "the item never landed"
wait_for 5 lock_gone && ok "the drain lock is released" || bad "the drain lock is still held"
[ -f "$KB_CAPTURE_SPOOL/other-sess.meta" ] && bad "meta removed with the item" || ok "meta removed with the item"

echo "== a fast replay stays synchronous (no background process) =="
reset; park
export FAKE_REPLAY_SECS=0
run_hook
landed_it && ok "small item lands before the hook exits" || bad "small item did not land synchronously"
[ "$(calls)" = 1 ] && ok "exactly one replay call" || bad "expected one replay call, got $(calls)"
[ -e "$KB_CAPTURE_SPOOL/.drain.lock" ] && bad "no drain lock for a synchronous landing" || ok "no drain lock for a synchronous landing"

echo "== a live drainer holds the lock: no second replay is started =="
reset; park
export FAKE_REPLAY_SECS=4
sleep 60 & HOLDER=$!
LIVE_PIDS="$HOLDER"
mkdir -p "$KB_CAPTURE_SPOOL/.drain.lock"
printf '%s' "$HOLDER" >"$KB_CAPTURE_SPOOL/.drain.lock/pid"
run_hook
sleep 1
# the synchronous slice itself is one call (it times out); a second call would
# be the detached drainer.
[ "$(calls)" = 1 ] && ok "no second replay while the lock is held by a live pid" || bad "a second replay started under a live lock ($(calls) calls)"
kill "$HOLDER" 2>/dev/null; LIVE_PIDS=""

echo "== a dead drainer's lock is reclaimed =="
reset; park
export FAKE_REPLAY_SECS=3
mkdir -p "$KB_CAPTURE_SPOOL/.drain.lock"
sleep 0.1 & DEAD=$!; wait "$DEAD" 2>/dev/null
printf '%s' "$DEAD" >"$KB_CAPTURE_SPOOL/.drain.lock/pid"
run_hook
wait_for 15 landed_it && ok "stale lock reclaimed and the item drained" || bad "stale lock blocked the drain"
wait_for 5 lock_gone && ok "reclaimed lock released at the end" || bad "reclaimed lock left behind"

echo "== the background drain is bounded =="
reset; park
export FAKE_REPLAY_SECS=60 KB_SPOOL_DRAIN_SECS=2
run_hook
sleep 1
[ -e "$KB_CAPTURE_SPOOL/.drain.lock" ] && ok "the drainer is running" || bad "the drainer never started"
wait_for 8 lock_gone && ok "the drainer is killed at KB_SPOOL_DRAIN_SECS and releases its lock" || bad "the drainer outlived its bound"
landed_it && bad "the bounded drain should not have landed (60s replay)" || ok "the over-long replay did not land"
unset KB_SPOOL_DRAIN_SECS

printf '\n%s passed, %s failed\n' "$PASS" "$FAIL"
[ "$FAIL" = 0 ]
