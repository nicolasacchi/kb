#!/usr/bin/env bash
# test-recall-scent.sh — CT-D1: kb-recall.sh's turn-1 SCENT branch.
#
# The contract under test, in one line: on the FIRST UserPromptSubmit of a
# session id the hook injects the `kb context` COUNTS line and nothing else;
# on every subsequent turn it is byte-identical to its pre-CT-D1 recall
# behaviour; and every honest failure mode (no session id, unwritable cache,
# an old/absent daemon, an empty corpus) falls back to recall rather than
# injecting nothing.
#
# Why the fallbacks matter more than the happy path: this hook runs on every
# prompt of every session on this machine. A scent branch that could swallow
# the recall block on a 404 would silently delete the memory injection during
# any rolling deploy — so each fallback gets its own case below.
#
# Fake `kb` on PATH (both `kb context` and `kb recall` are exercised); `jq`
# is real.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-recall-scent.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RECALL="$HOOKS_DIR/kb-recall.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-recall-scent-test.XXXXXX")"
cleanup() { chmod -R u+w "$TMPROOT" 2>/dev/null; rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

mkdir -p "$TMPROOT/bin"
export PATH="$TMPROOT/bin:$PATH"
export XDG_CACHE_HOME="$TMPROOT/cache"

# A `kb` that serves BOTH verbs: `context` returns a pack with a scent line,
# `recall` returns one ordinary hit. Any test that sees the recall shape
# therefore proves the scent branch was not taken.
write_kb_both() {
  cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "context" ]; then
  echo '{"q":"x","scent":"3 prior sessions · 2 open comments · 5 memories","memories":[],"sessions":[],"comments":[],"code_hints":[]}'
  exit 0
fi
if [ "$1" = "recall" ]; then
  echo '{"hits":[{"title":"T1","kb":"main","id":"abc123def456"}]}'
  exit 0
fi
exit 0
EOF
  chmod +x "$TMPROOT/bin/kb"
}

echo "== kb-recall.sh CT-D1 scent-branch test matrix =="
echo "tmp root: $TMPROOT"
echo

# --- 1. FIRST turn of a session -> the counts line, and ONLY counts -------
write_kb_both
out="$(printf '%s' '{"session_id":"sc-1","cwd":"/tmp","prompt":"wire the thing"}' | "$RECALL")"
case "$out" in
  *"3 prior sessions · 2 open comments · 5 memories"*)
    ok "first turn injects the scent counts line" ;;
  *) bad "first turn injects the scent counts line (got: $out)" ;;
esac
case "$out" in
  *"run \`kb context"*|*'kb context \"<your task>\"'*)
    ok "the scent names the verb to pull with" ;;
  *) bad "the scent names the verb to pull with (got: $out)" ;;
esac
# The scent is ADDITIVE (orchestrator ruling, 2026-08-22): turn 1 keeps the
# ordinary recall block — memories have been pushed every turn since v0.9 and
# R0/R3 governs EPISODIC material, not them — and appends the scent beneath
# it. What must NOT ride along is episodic SUBSTANCE: session bodies or
# transcript excerpts, which stay behind the `kb context` pull.
case "$out" in
  *"Relevant memories from kb"*"kb has prior context for this task"*)
    ok "turn 1 keeps the recall block AND appends the scent" ;;
  *) bad "turn 1 keeps the recall block AND appends the scent (got: $out)" ;;
esac
case "$out" in
  *"- T1  [main]"*)
    ok "turn 1 still carries its memory titles (no regression)" ;;
  *) bad "turn 1 still carries its memory titles (got: $out)" ;;
esac

# --- 2. SECOND turn of the SAME session -> recall, byte-identical ---------
out2="$(printf '%s' '{"session_id":"sc-1","cwd":"/tmp","prompt":"and now this"}' | "$RECALL")"
case "$out2" in
  *"Relevant memories from kb (recall — these persist across sessions):"*"- T1  [main]"*)
    ok "turn 2 is the unchanged recall block" ;;
  *) bad "turn 2 is the unchanged recall block (got: $out2)" ;;
esac
case "$out2" in
  *"3 prior sessions"*) bad "turn 2 must NOT re-emit the scent (got: $out2)" ;;
  *) ok "the scent fires exactly once per session id" ;;
esac

# --- 3. a THIRD turn is still recall (the marker is not consumed) ---------
out3="$(printf '%s' '{"session_id":"sc-1","cwd":"/tmp","prompt":"third"}' | "$RECALL")"
case "$out3" in
  *"- T1  [main]"*) ok "turn 3 is still the recall block" ;;
  *) bad "turn 3 is still the recall block (got: $out3)" ;;
esac

# --- 4. a DIFFERENT session id gets its own first turn -------------------
out4="$(printf '%s' '{"session_id":"sc-2","cwd":"/tmp","prompt":"new session"}' | "$RECALL")"
case "$out4" in
  *"3 prior sessions · 2 open comments · 5 memories"*)
    ok "a new session id gets its own scent turn" ;;
  *) bad "a new session id gets its own scent turn (got: $out4)" ;;
esac

# --- 5. an EMPTY corpus ("no prior context") falls back to recall ---------
#        — a scent that says nothing would be a nag, not a signal.
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "context" ]; then
  echo '{"q":"x","scent":"no prior context","memories":[],"sessions":[],"comments":[],"code_hints":[]}'
  exit 0
fi
if [ "$1" = "recall" ]; then
  echo '{"hits":[{"title":"T1","kb":"main","id":"abc123def456"}]}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
out5="$(printf '%s' '{"session_id":"sc-3","cwd":"/tmp","prompt":"empty corpus"}' | "$RECALL")"
case "$out5" in
  *"no prior context"*) bad "an empty pack must not be injected (got: $out5)" ;;
  *) ok "\"no prior context\" is never injected" ;;
esac
case "$out5" in
  *"- T1  [main]"*)
    ok "an empty pack falls back to the recall block" ;;
  *) bad "an empty pack falls back to the recall block (got: $out5)" ;;
esac

# --- 5b. SCENT-ONLY: recall returns nothing, but prior context exists -----
#         The branch the additive shape newly enables. Before the ruling an
#         empty recall window exited silently and the operator never learned
#         that prior sessions/comments existed at all.
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "context" ]; then
  echo '{"q":"x","scent":"2 prior sessions · 1 open comment","memories":[],"sessions":[],"comments":[],"code_hints":[]}'
  exit 0
fi
if [ "$1" = "recall" ]; then
  echo '{"hits":[]}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
out5b="$(printf '%s' '{"session_id":"sc-3b","cwd":"/tmp","prompt":"nothing recalled"}' | "$RECALL")"
case "$out5b" in
  *"2 prior sessions · 1 open comment"*)
    ok "an empty recall window still emits the scent alone" ;;
  *) bad "an empty recall window still emits the scent alone (got: $out5b)" ;;
esac
case "$out5b" in
  *"Relevant memories from kb"*)
    bad "scent-only must not fake an empty recall header (got: $out5b)" ;;
  *) ok "scent-only carries no empty recall header" ;;
esac

# --- 5c. BOTH empty -> silence, exactly as before -------------------------
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "context" ]; then
  echo '{"q":"x","scent":"no prior context","memories":[],"sessions":[],"comments":[],"code_hints":[]}'
  exit 0
fi
if [ "$1" = "recall" ]; then
  echo '{"hits":[]}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
out5c="$(printf '%s' '{"session_id":"sc-3c","cwd":"/tmp","prompt":"nothing at all"}' | "$RECALL")"
if [ -z "$out5c" ]; then
  ok "nothing to say -> silence (unchanged)"
else
  bad "nothing to say -> silence (got: $out5c)"
fi

# --- 6. a daemon too OLD to serve /api/context falls back to recall -------
#        (the rolling-deploy case: `kb context` fails, recall still works)
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "context" ]; then
  echo 'context failed: HTTP 404 — not found' >&2
  exit 1
fi
if [ "$1" = "recall" ]; then
  echo '{"hits":[{"title":"T1","kb":"main","id":"abc123def456"}]}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
out6="$(printf '%s' '{"session_id":"sc-4","cwd":"/tmp","prompt":"old daemon"}' | "$RECALL")"
case "$out6" in
  *"- T1  [main]"*)
    ok "a 404 on /api/context falls back to recall (rolling deploy safe)" ;;
  *) bad "a 404 on /api/context falls back to recall (got: $out6)" ;;
esac

# --- 7. NO session id -> no marker is possible -> recall ------------------
write_kb_both
out7="$(printf '%s' '{"cwd":"/tmp","prompt":"no session id at all"}' | "$RECALL")"
case "$out7" in
  *"- T1  [main]"*)
    ok "a payload with no session_id falls back to recall" ;;
  *) bad "a payload with no session_id falls back to recall (got: $out7)" ;;
esac
case "$out7" in
  *"3 prior sessions"*) bad "no session id must not emit a scent (got: $out7)" ;;
  *) ok "no session id -> no scent (the first-turn proxy needs one)" ;;
esac

# --- 8. an UNWRITABLE cache dir -> recall (never a scent every turn) ------
UNWRITABLE="$TMPROOT/ro"
mkdir -p "$UNWRITABLE"
chmod 500 "$UNWRITABLE"
out8="$(printf '%s' '{"session_id":"sc-5","cwd":"/tmp","prompt":"ro cache"}' \
  | XDG_CACHE_HOME="$UNWRITABLE" "$RECALL")"
chmod 700 "$UNWRITABLE"
case "$out8" in
  *"- T1  [main]"*)
    ok "an unwritable cache dir falls back to recall (not a scent every turn)" ;;
  *) bad "an unwritable cache dir falls back to recall (got: $out8)" ;;
esac

# --- 9. kimi format: bare stdout, same scent text -------------------------
write_kb_both
out9="$(printf '%s' '{"session_id":"sc-6","cwd":"/tmp","prompt":[{"type":"text","text":"kimi turn one"}]}' \
  | KB_HOOK_FMT=kimi "$RECALL")"
case "$out9" in
  *"hookSpecificOutput"*) bad "kimi format must print the block bare (got: $out9)" ;;
  *) ok "kimi format prints the scent bare (no envelope)" ;;
esac
case "$out9" in
  *"3 prior sessions · 2 open comments · 5 memories"*)
    ok "kimi format carries the same scent text" ;;
  *) bad "kimi format carries the same scent text (got: $out9)" ;;
esac

# --- 10. the scent branch forwards --cwd and --session to `kb context` ----
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "context" ]; then
  printf '%s\n' "$@" >"$KB_ARGV_SPY"
  echo '{"scent":"1 memory"}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
export KB_ARGV_SPY="$TMPROOT/argv.txt"
printf '%s' '{"session_id":"sc-7","cwd":"/home/user/project/kb","prompt":"spy"}' | "$RECALL" >/dev/null
spy="$(cat "$KB_ARGV_SPY" 2>/dev/null)"
case "$spy" in
  *"--cwd"*"/home/user/project/kb"*) ok "the scent call forwards --cwd" ;;
  *) bad "the scent call forwards --cwd (got: $spy)" ;;
esac
case "$spy" in
  *"--session"*"sc-7"*) ok "the scent call forwards --session (no self-reporting)" ;;
  *) bad "the scent call forwards --session (got: $spy)" ;;
esac
case "$spy" in
  *"--json"*) ok "the scent call asks for --json" ;;
  *) bad "the scent call asks for --json (got: $spy)" ;;
esac

# --- 11. a cwd containing a SPACE stays ONE argv slot ---------------------
printf '%s' '{"session_id":"sc-8","cwd":"/home/user/my projects/kb","prompt":"spaces"}' | "$RECALL" >/dev/null
spy2="$(cat "$KB_ARGV_SPY" 2>/dev/null)"
# The spy prints one arg per line; the cwd must survive as a single line.
if printf '%s\n' "$spy2" | grep -qx '/home/user/my projects/kb'; then
  ok "a cwd with a space stays one argv slot"
else
  bad "a cwd with a space stays one argv slot (got: $spy2)"
fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
