#!/usr/bin/env bash
# test-capture-replay-order.sh - omp capture round 5: the capture spool is
# SHARED by every session and every adapter, and a successful landing of
# session Y replays EVERY pending item, including session X's. A replayed
# snapshot older than a capture that already landed must never overwrite it.
#
# Sequence (the reported incident): X is spooled at M1 -> X appends M2 and lands
# it fresh -> a replay (driven here the way hook_adapter_land does, by Y landing)
# reads X's stale M1 item. The corpus must still hold M2, no second file may
# appear for X, and the stale item must be gone from the spool (dropped, not
# retried forever). A snapshot NEWER than the corpus file must still replay.
#
# Needs the REAL kb built from this tree (KB_BIN_DIR): the rule lives in the
# Rust writer (`kb sessions capture --replay-spool`). A kb without the rule
# fails the first case - that is this test's job.
#
# Runnable standalone:  KB_BIN_DIR=<dir with kb> bash plugins/kb-memory/hooks/tests/test-capture-replay-order.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
[ -n "${KB_BIN_DIR:-}" ] && PATH="$KB_BIN_DIR:$PATH"

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

if ! command -v kb >/dev/null 2>&1; then
  if [ -n "${CI:-}" ]; then echo "not ok  - kb is required in CI"; exit 1; fi
  echo "SKIP: no kb binary"; echo "passed=0 failed=0 skipped=1"; exit 0
fi

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-replay-order.XXXXXX")"
trap 'rm -rf "$TMPROOT"' EXIT
export HOME="$TMPROOT/home" XDG_CACHE_HOME="$TMPROOT/cache"
unset KB_CACHE_DIR KB_STATE_DIR KB_CONFIG_DIR
export KB_SESSIONS_DIR="$TMPROOT/sessions" KB_CAPTURE_SPOOL="$TMPROOT/spool" KB_CAPTURE_LOCKS="$TMPROOT/locks"
mkdir -p "$HOME" "$KB_SESSIONS_DIR"
# shellcheck disable=SC1091
. "$HOOKS_DIR/kb-hook-lib.sh"

rec() { # <sid> <role> <text>
  printf '{"sessionId":"%s","type":"%s","timestamp":"2026-03-01T09:00:00.000Z","message":{"role":"%s","content":"%s"}}\n' "$1" "$2" "$2" "$3"
}
n_html() { find "$KB_SESSIONS_DIR" -name 'session-*.html' | wc -l | tr -d ' '; }
html_of() { find "$KB_SESSIONS_DIR" -name "session-*-$1.html" | head -1; }
spool_items() { find "$KB_CAPTURE_SPOOL" -maxdepth 1 -name '*.jsonl' 2>/dev/null | wc -l | tr -d ' '; }

X=ses-replay-x
Y=ses-replay-y
rec "$X" user "m1-first" >"$TMPROOT/x1.jsonl"
{ cat "$TMPROOT/x1.jsonl"; rec "$X" assistant "m2-fresh-marker"; } >"$TMPROOT/x2.jsonl"
rec "$Y" user "y-only" >"$TMPROOT/y.jsonl"

echo "== stale spooled snapshot vs a fresher landed capture =="
# X lands M2 fresh through the real writer, while X's M1 snapshot (parked
# earlier by a failed capture, so older than that landing) is still in the
# shared spool - exactly what a replay that started before the landing sees.
kb sessions capture --transcript "$TMPROOT/x2.jsonl" --session-id "$X" --out "$KB_SESSIONS_DIR" >/dev/null 2>&1
hook_spool_put "$TMPROOT/x1.jsonl" "$X" "" "" omp
touch -d '-60 seconds' "$KB_CAPTURE_SPOOL"/*.jsonl
XF="$(html_of "$X")"
if [ -n "$XF" ] && grep -q m2-fresh-marker "$XF"; then ok "X landed M2 fresh"; else bad "X landed M2 fresh"; fi
before="$(sha256sum "$XF" | cut -d' ' -f1)"
# ... and Y landing replays the whole spool, X's stale item included.
hook_adapter_land "$Y" "$TMPROOT/y.jsonl" "" "" omp
if [ "$(sha256sum "$XF" | cut -d' ' -f1)" = "$before" ] && grep -q m2-fresh-marker "$XF"; then
  ok "a replay never publishes a stale snapshot over a fresher capture"
else
  bad "a replay overwrote X's fresher capture with its stale M1 snapshot"
fi
[ "$(n_html)" = 2 ] && ok "no duplicate capture file for X" || bad "unexpected html count: $(n_html)"
[ "$(spool_items)" = 0 ] && ok "the stale item is dropped from the spool (not retried forever)" || bad "spool still holds $(spool_items) item(s)"

echo "== a snapshot newer than the landed capture still replays =="
Z=ses-replay-z
rec "$Z" user "z-old" >"$TMPROOT/z1.jsonl"
{ cat "$TMPROOT/z1.jsonl"; rec "$Z" assistant "z-newer-marker"; } >"$TMPROOT/z2.jsonl"
kb sessions capture --transcript "$TMPROOT/z1.jsonl" --session-id "$Z" --out "$KB_SESSIONS_DIR" >/dev/null 2>&1
sleep 1.2
hook_spool_put "$TMPROOT/z2.jsonl" "$Z" "" "" omp
kb sessions capture --replay-spool --out "$KB_SESSIONS_DIR" >/dev/null 2>&1
ZF="$(html_of "$Z")"
if [ -n "$ZF" ] && grep -q z-newer-marker "$ZF" && [ "$(spool_items)" = 0 ]; then
  ok "a fresher spooled snapshot is replayed and removed"
else
  bad "fresher spooled snapshot was not replayed"
fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
