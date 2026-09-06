#!/usr/bin/env bash
# test-distill-nudge-kimi.sh — self-contained test matrix for
# kb-distill-nudge-kimi.sh (MI-W0.3): the kimi-side twin of
# kb-distill-nudge-codex.sh, reading the Kimi Code wire.jsonl encoding
# (metadata / turn.prompt / context.append_loop_event tool.call+tool.result)
# instead of a Claude transcript or codex rollout. Runs the REAL script in
# hook mode against the checked-in synthesized fixtures laid out in the
# real on-disk shape ($KIMI_CODE_HOME/sessions/<workDirKey>/<sid>/agents/
# main/wire.jsonl). `jq` is real; `kb` is never called by this hook.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-distill-nudge-kimi.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
NUDGE="$HOOKS_DIR/kb-distill-nudge-kimi.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-distill-nudge-kimi-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

export KB_SESSIONS_DIR="$TMPROOT/sessions"
mkdir -p "$KB_SESSIONS_DIR"
export KIMI_CODE_HOME="$TMPROOT/kimi-home"
CWD="$TMPROOT/proj"
mkdir -p "$CWD"

# plant <fixture> as the wire file for session <sid> (real on-disk layout).
plant() {
  local fixture="$1" sid="$2"
  local wdkey dir
  wdkey="wd_$(basename -- "$CWD")_$(printf '%s' "$CWD" | sha256sum | cut -c1-12)"
  dir="$KIMI_CODE_HOME/sessions/$wdkey/$sid/agents/main"
  mkdir -p "$dir"
  cp "$fixture" "$dir/wire.jsonl"
}

run_nudge() {
  local sid="$1" cache="$2"
  export XDG_CACHE_HOME="$cache"
  mkdir -p "$cache"
  printf '%s' "{\"session_id\":\"$sid\",\"cwd\":\"$CWD\",\"hook_event_name\":\"Stop\"}" | "$NUDGE"
}

SID_COMMIT="session_aaaa0000-1111-2222-3333-444444444444"
SID_REMEMBERED="session_bbbb0000-1111-2222-3333-444444444444"
plant "$SCRIPT_DIR/fixtures/kimi-wire-commit.jsonl" "$SID_COMMIT"
plant "$SCRIPT_DIR/fixtures/kimi-wire-remembered.jsonl" "$SID_REMEMBERED"

echo "== kb-distill-nudge-kimi.sh test matrix (MI-W0.3) =="
echo "tmp root: $TMPROOT"
echo

out="$(run_nudge "$SID_COMMIT" "$TMPROOT/cache-fire")"
if printf '%s' "$out" | grep -q "kb: this session has commits but no curated memory" \
   && printf '%s' "$out" | grep -qF "$SID_COMMIT"; then
  ok "kimi commit, no success marker -> plain-text nudge fires"
else
  bad "kimi commit, no success marker -> plain-text nudge fires (got: $out)"
fi

if [ -f "$TMPROOT/cache-fire/kb/distill-nudged-kimi-${SID_COMMIT//_/-}" ]; then
  ok "once-per-session marker created"
else
  bad "once-per-session marker created (dir: $(ls "$TMPROOT/cache-fire/kb" 2>/dev/null))"
fi

ledger="$TMPROOT/cache-fire/kb/distill-pending"
if [ -f "$ledger" ] && [ "$(grep -c "^kimi $SID_COMMIT [0-9]" "$ledger")" = "1" ]; then
  ok "ledger has one 'kimi <sid> <epoch>' line"
else
  bad "ledger has one 'kimi <sid> <epoch>' line (ledger: $(cat "$ledger" 2>/dev/null))"
fi

# Second run against the same session+cache must stay silent (marker cap)
# and must not duplicate the ledger line.
out2="$(run_nudge "$SID_COMMIT" "$TMPROOT/cache-fire")"
if [ -z "$out2" ]; then
  ok "marker caps the nudge at once per session"
else
  bad "marker caps the nudge at once per session (got: $out2)"
fi
if [ "$(grep -c "^kimi $SID_COMMIT " "$ledger")" = "1" ]; then
  ok "second run does not duplicate the ledger line"
else
  bad "second run does not duplicate the ledger line (ledger: $(cat "$ledger"))"
fi

# Commit + successful kb remember -> no nudge, no ledger entry.
out3="$(run_nudge "$SID_REMEMBERED" "$TMPROOT/cache-remembered")"
if [ -z "$out3" ]; then
  ok "kimi commit + success marker -> suppressed"
else
  bad "kimi commit + success marker -> suppressed (got: $out3)"
fi
if [ ! -f "$TMPROOT/cache-remembered/kb/distill-pending" ]; then
  ok "suppressed run queues no ledger entry"
else
  bad "suppressed run queues no ledger entry (ledger: $(cat "$TMPROOT/cache-remembered/kb/distill-pending"))"
fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
