#!/usr/bin/env bash
# test-grok-distill-pending.sh — self-contained test matrix for the
# MI-W0.3 grok distill-pending relay: kb-capture-grok.sh's
# queue_distill_pending (append-on-hit + dedup by session id, on either
# write path) and kb-wake.sh's ledger surface+consume (drop entries older
# than 14 days, surface up to the 3 newest, rewrite the ledger to hold
# only what wasn't surfaced).
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-grok-distill-pending.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
GROK_CAPTURE="$HOOKS_DIR/kb-capture-grok.sh"
WAKE="$HOOKS_DIR/kb-wake.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-distill-pending-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

mkdir -p "$TMPROOT/bin"
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
# `kb sessions capture` fails -> forces kb-capture-grok.sh's bash-fallback
# write path, so this test matrix exercises "cover the success of
# either" write path (see test 1b below for the primary-path case).
if [ "$1" = "sessions" ] && [ "$2" = "capture" ]; then exit 1; fi
if [ "$1" = "recall" ]; then echo '{"hits":[]}'; exit 0; fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
export PATH="$TMPROOT/bin:$PATH"

# --- fixture: a grok session dir with a commit and no successful remember --
FIXDIR="$TMPROOT/grok-session-commit"
mkdir -p "$FIXDIR"
cat >"$FIXDIR/summary.json" <<'EOF'
{"info": {"id": "01900000-0000-7000-8000-000000000099", "cwd": "/tmp/kb-grok-fixture-cwd2"},
 "created_at": "2026-08-01T00:00:00.000Z", "updated_at": "2026-08-01T00:05:00.000Z",
 "generated_title": "Commit without remember"}
EOF
cat >"$FIXDIR/chat_history.jsonl" <<'EOF'
{"type":"user","content":[{"type":"text","text":"commit the fix"}]}
{"type":"assistant","content":"Committing now.","tool_calls":[{"id":"call-c1","name":"run_terminal_command","arguments":"{\"command\":\"git commit -m 'fix'\"}"}],"model_id":"grok-4.5-fixture"}
{"type":"tool_result","tool_call_id":"call-c1","content":"[main abc9999] fix"}
{"type":"assistant","content":"Done.","tool_calls":[]}
EOF

# --- 1a. capture (bash-fallback write path) with commit-no-success ---------
export KB_SESSIONS_DIR="$TMPROOT/grok-sessions"
export XDG_CACHE_HOME="$TMPROOT/cache1"
mkdir -p "$KB_SESSIONS_DIR"
LEDGER="$XDG_CACHE_HOME/kb/distill-pending"

"$GROK_CAPTURE" --session-dir "$FIXDIR" --cwd /tmp/kb-grok-fixture-cwd2 >/dev/null 2>&1
if [ -f "$LEDGER" ] && [ "$(grep -c '^grok grok-session-commit ' "$LEDGER")" = "1" ]; then
  ok "commit-no-success capture (bash-fallback path) queues one ledger line"
else
  bad "commit-no-success capture (bash-fallback path) queues one ledger line (ledger: $(cat "$LEDGER" 2>/dev/null))"
fi

# --- 1b. same, but via the PRIMARY `kb sessions capture` write path --------
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "sessions" ] && [ "$2" = "capture" ]; then exit 0; fi
if [ "$1" = "recall" ]; then echo '{"hits":[]}'; exit 0; fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
export KB_SESSIONS_DIR="$TMPROOT/grok-sessions-primary"
export XDG_CACHE_HOME="$TMPROOT/cache1b"
mkdir -p "$KB_SESSIONS_DIR"
LEDGER_1B="$XDG_CACHE_HOME/kb/distill-pending"
"$GROK_CAPTURE" --session-dir "$FIXDIR" --cwd /tmp/kb-grok-fixture-cwd2 >/dev/null 2>&1
if [ -f "$LEDGER_1B" ] && [ "$(grep -c '^grok grok-session-commit ' "$LEDGER_1B")" = "1" ]; then
  ok "commit-no-success capture (primary write path) also queues one ledger line"
else
  bad "commit-no-success capture (primary write path) also queues one ledger line (ledger: $(cat "$LEDGER_1B" 2>/dev/null))"
fi
# restore the fallback-forcing fake kb for the rest of the matrix
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "sessions" ] && [ "$2" = "capture" ]; then exit 1; fi
if [ "$1" = "recall" ]; then echo '{"hits":[]}'; exit 0; fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"

# --- 2. re-run (retry / resumed job) -> no duplicate ledger line ----------
export KB_SESSIONS_DIR="$TMPROOT/grok-sessions"
export XDG_CACHE_HOME="$TMPROOT/cache1"
"$GROK_CAPTURE" --session-dir "$FIXDIR" --cwd /tmp/kb-grok-fixture-cwd2 >/dev/null 2>&1
if [ "$(grep -c '^grok grok-session-commit ' "$LEDGER")" = "1" ]; then
  ok "re-capture does not duplicate the ledger line"
else
  bad "re-capture does not duplicate the ledger line (ledger: $(cat "$LEDGER" 2>/dev/null))"
fi

# --- 3. a capture with NO commit -> no ledger entry ------------------------
export KB_SESSIONS_DIR="$TMPROOT/grok-sessions-nocommit"
export XDG_CACHE_HOME="$TMPROOT/cache-nocommit"
mkdir -p "$KB_SESSIONS_DIR"
NOCOMMIT_DIR="$TMPROOT/grok-session-nocommit"
mkdir -p "$NOCOMMIT_DIR"
cat >"$NOCOMMIT_DIR/summary.json" <<'EOF'
{"info": {"id": "01900000-0000-7000-8000-0000000000aa", "cwd": "/tmp/kb-grok-fixture-cwd3"},
 "created_at": "2026-08-01T00:00:00.000Z", "updated_at": "2026-08-01T00:05:00.000Z"}
EOF
cat >"$NOCOMMIT_DIR/chat_history.jsonl" <<'EOF'
{"type":"user","content":[{"type":"text","text":"just read a file"}]}
{"type":"assistant","content":"Reading now.","tool_calls":[{"id":"call-r1","name":"read_file","arguments":"{\"target_file\":\"README.md\"}"}],"model_id":"grok-4.5-fixture"}
{"type":"tool_result","tool_call_id":"call-r1","content":"# hi"}
{"type":"assistant","content":"Done.","tool_calls":[]}
EOF
"$GROK_CAPTURE" --session-dir "$NOCOMMIT_DIR" --cwd /tmp/kb-grok-fixture-cwd3 >/dev/null 2>&1
if [ ! -f "$XDG_CACHE_HOME/kb/distill-pending" ]; then
  ok "no-commit capture queues nothing"
else
  bad "no-commit capture queues nothing (ledger: $(cat "$XDG_CACHE_HOME/kb/distill-pending"))"
fi

# --- 4. kb-wake.sh surfaces the 2 fresh entries, drops the 1 stale one,
#        and consumes (rewrites) the ledger -------------------------------
export XDG_CACHE_HOME="$TMPROOT/cache-wake"
mkdir -p "$XDG_CACHE_HOME/kb"
now="$(date +%s)"
WAKE_LEDGER="$XDG_CACHE_HOME/kb/distill-pending"
{
  printf 'grok fresh-sid-1 %s\n' "$((now - 3600))"
  printf 'grok fresh-sid-2 %s\n' "$((now - 7200))"
  printf 'grok stale-sid-3 %s\n' "$((now - 20 * 86400))"
} >"$WAKE_LEDGER"

out="$(printf '%s' '{"session_id":"wake-test","cwd":"/tmp"}' | "$WAKE")"
ctx="$(printf '%s' "$out" | jq -r '.hookSpecificOutput.additionalContext' 2>/dev/null)"
case "$ctx" in
  *"Pending distill (grok): 2 session(s)"*"fresh-sid-1"*"fresh-sid-2"*)
    ok "kb-wake.sh surfaces the 2 fresh entries" ;;
  *)
    bad "kb-wake.sh surfaces the 2 fresh entries (ctx: $ctx)" ;;
esac
case "$ctx" in
  *"stale-sid-3"*) bad "kb-wake.sh never surfaces the stale entry" ;;
  *) ok "kb-wake.sh never surfaces the stale entry" ;;
esac
if [ ! -s "$WAKE_LEDGER" ]; then
  ok "ledger left empty of surfaced+stale entries after surfacing"
else
  bad "ledger left empty of surfaced+stale entries after surfacing (ledger: $(cat "$WAKE_LEDGER"))"
fi

# --- 5. no ledger file at all -> kb-wake.sh totally unaffected -------------
export XDG_CACHE_HOME="$TMPROOT/cache-wake-none"
mkdir -p "$XDG_CACHE_HOME"
out5="$(printf '%s' '{"session_id":"wake-none","cwd":"/tmp"}' | "$WAKE")"
ctx5="$(printf '%s' "$out5" | jq -r '.hookSpecificOutput.additionalContext' 2>/dev/null)"
case "$ctx5" in
  *"Pending distill"*) bad "no ledger file -> no pending block" ;;
  *) ok "no ledger file -> no pending block" ;;
esac

# --- 6. only-stale entries -> no block, ledger left untouched (no
#        surface event happened, so nothing is consumed) -------------------
export XDG_CACHE_HOME="$TMPROOT/cache-wake-stale"
mkdir -p "$XDG_CACHE_HOME/kb"
STALE_LEDGER="$XDG_CACHE_HOME/kb/distill-pending"
printf 'grok only-stale %s\n' "$((now - 20 * 86400))" >"$STALE_LEDGER"
out6="$(printf '%s' '{"session_id":"wake-stale","cwd":"/tmp"}' | "$WAKE")"
ctx6="$(printf '%s' "$out6" | jq -r '.hookSpecificOutput.additionalContext' 2>/dev/null)"
case "$ctx6" in
  *"Pending distill"*) bad "only-stale ledger -> no pending block" ;;
  *) ok "only-stale ledger -> no pending block" ;;
esac
if [ "$(cat "$STALE_LEDGER")" = "grok only-stale $((now - 20 * 86400))" ]; then
  ok "only-stale ledger left untouched"
else
  bad "only-stale ledger left untouched (ledger: $(cat "$STALE_LEDGER"))"
fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
