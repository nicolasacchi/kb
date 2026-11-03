#!/usr/bin/env bash
# test-lib-sandbox.sh - AH1: lib-sandbox.sh's kb_test_sandbox keeps a spool
# parked in a developer's HOME out of reach of a real `kb sessions capture
# --replay-spool`.
#
# The incident: a hook test ran the real kb with the developer's HOME, a landed
# capture made hook_adapter_land replay the default spool, the replay deleted
# every item it landed, and the test's EXIT trap deleted the landing dir. Here
# the "developer's home" is a FAKEHOME inside TMPROOT, so this test never
# touches a real home, even when the helper is broken.
#
# Needs the REAL `kb` binary (KB_BIN_DIR, set by the cargo harness).
# Runnable standalone:  KB_BIN_DIR=<dir with kb> bash plugins/kb-memory/hooks/tests/test-lib-sandbox.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-lib-sandbox-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }
finish() {
  printf 'passed=%s failed=%s\n' "$PASS" "$FAIL"
  [ "$FAIL" -eq 0 ]
}

echo "== simulate an unsandboxed developer shell =="
FAKEHOME="$TMPROOT/fakehome"
mkdir -p "$FAKEHOME"
unset XDG_CACHE_HOME XDG_CONFIG_HOME XDG_DATA_HOME XDG_STATE_HOME
export HOME="$FAKEHOME"
export KB_HOME="$FAKEHOME/kb-home"
export KB_CACHE_DIR="$FAKEHOME/.cache/kb"
export KB_CAPTURE_SPOOL="$FAKEHOME/.cache/kb/capture-spool"
export KB_CAPTURE_LOCKS="$FAKEHOME/.cache/kb/capture-locks"
export KB_SESSIONS_DIR="$FAKEHOME/sessions"
# shellcheck source=../kb-hook-lib.sh
. "$HOOKS_DIR/kb-hook-lib.sh"

SID="sess-sandbox-0001"
T="$TMPROOT/t.jsonl"
cat >"$T" <<JSONL
{"sessionId":"$SID","type":"user","timestamp":"2026-03-01T09:00:00.000Z","message":{"role":"user","content":"parked before the sandbox"},"promptSource":"typed"}
JSONL
hook_spool_put "$T" "$SID" "$TMPROOT"
PARKED="$FAKEHOME/.cache/kb/capture-spool"
if [ -f "$PARKED/$SID.jsonl" ] && [ -f "$PARKED/$SID.meta" ]; then
  ok "precondition: the item is parked in the hostile shell's spool"
  cp "$PARKED/$SID.jsonl" "$TMPROOT/pristine.jsonl"
  cp "$PARKED/$SID.meta" "$TMPROOT/pristine.meta"
else
  bad "precondition: the item is parked in the hostile shell's spool"
  finish
  exit
fi

echo "== kb_test_sandbox moves every state root =="
. "$SCRIPT_DIR/lib-sandbox.sh"
kb_test_sandbox "$TMPROOT/sbx" || exit 1
for v in HOME XDG_CACHE_HOME XDG_CONFIG_HOME XDG_DATA_HOME XDG_STATE_HOME; do
  case "${!v:-}" in
    "$TMPROOT/sbx/"*) ok "$v points into the sandbox" ;;
    *) bad "$v points into the sandbox (${!v:-unset})" ;;
  esac
done
for v in KB_HOME KB_CACHE_DIR KB_STATE_DIR KB_CONFIG_DIR KB_CAPTURE_SPOOL KB_CAPTURE_LOCKS KB_SESSIONS_DIR; do
  if [ -z "${!v+x}" ]; then ok "$v is unset"; else bad "$v is unset (${!v})"; fi
done
case "$(hook_spool_dir)" in
  "$TMPROOT/sbx/"*) ok "hook_spool_dir resolves inside the sandbox" ;;
  *) bad "hook_spool_dir resolves inside the sandbox ($(hook_spool_dir))" ;;
esac
case "$(hook_capture_lock_dir)" in
  "$TMPROOT/sbx/"*) ok "hook_capture_lock_dir resolves inside the sandbox" ;;
  *) bad "hook_capture_lock_dir resolves inside the sandbox ($(hook_capture_lock_dir))" ;;
esac

echo "== real kb required =="
if [ -n "${KB_BIN_DIR:-}" ] && [ -x "$KB_BIN_DIR/kb" ]; then
  ok "a real kb binary is available (KB_BIN_DIR survived the sandbox)"
else
  bad "a real kb binary is required (KB_BIN_DIR)"
  finish
  exit
fi
PATH="$KB_BIN_DIR:$PATH"

echo "== control: the parked item can be landed =="
mkdir -p "$TMPROOT/control-spool" "$TMPROOT/control-out"
cp "$TMPROOT/pristine.jsonl" "$TMPROOT/control-spool/$SID.jsonl"
cp "$TMPROOT/pristine.meta" "$TMPROOT/control-spool/$SID.meta"
KB_CAPTURE_SPOOL="$TMPROOT/control-spool" kb sessions capture --replay-spool --out "$TMPROOT/control-out" >/dev/null 2>&1
n="$(find "$TMPROOT/control-out" -name 'session-*.html' 2>/dev/null | wc -l | tr -d ' ')"
[ "$n" = 1 ] && ok "control: exactly one capture landed" || bad "control: exactly one capture landed ($n)"
[ -f "$TMPROOT/control-spool/$SID.jsonl" ] && bad "control: the landed item was deleted" || ok "control: the landed item was deleted"

echo "== sandboxed replay leaves the parked item alone =="
mkdir -p "$TMPROOT/out"
kb sessions capture --replay-spool --out "$TMPROOT/out" >/dev/null 2>&1
cmp -s "$PARKED/$SID.jsonl" "$TMPROOT/pristine.jsonl" && ok "parked .jsonl is byte-identical" || bad "parked .jsonl is byte-identical"
cmp -s "$PARKED/$SID.meta" "$TMPROOT/pristine.meta" && ok "parked .meta is byte-identical" || bad "parked .meta is byte-identical"
n="$(find "$TMPROOT/out" -name 'session-*.html' 2>/dev/null | wc -l | tr -d ' ')"
[ "$n" = 0 ] && ok "nothing landed from the unsandboxed spool" || bad "nothing landed from the unsandboxed spool ($n)"

finish
