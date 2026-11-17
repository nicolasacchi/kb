#!/usr/bin/env bash
# test-recall-context.sh — SG-Q2: kb-recall.sh's opt-in KB_RECALL_CONTEXT=1.
#
#   1. Flag off: no --transcript anywhere (calls byte-identical to before).
#   2. Flag on + readable .transcript_path: `kb recall` gets --transcript <path>.
#   3. Flag on + missing file (or no path): no --transcript.
#   4. Flag on + KB_TURN=1: `kb turn` gets --transcript <path>.
#   The prompt argument always stays .prompt.
#
# Fake `kb` on PATH logs its argv; `jq` is real.
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-recall-context.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RECALL="$HOOKS_DIR/kb-recall.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-recall-context-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

mkdir -p "$TMPROOT/bin"
export LOG="$TMPROOT/calls.log"
cat >"$TMPROOT/bin/kb" <<'EOF2'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$LOG"
case "$1" in
  turn) printf '%s' '{"text":"T","recalled":[]}'; exit 0 ;;
  recall) printf '%s' '{"hits":[{"title":"OLD","kb":"main","id":"abc123def456"}]}'; exit 0 ;;
  context) printf '%s' '{"scent":"no prior context"}'; exit 0 ;;
esac
exit 0
EOF2
chmod +x "$TMPROOT/bin/kb"
export PATH="$TMPROOT/bin:$PATH"
export XDG_CACHE_HOME="$TMPROOT/cache"

TR="$TMPROOT/transcript.jsonl"
echo '{}' >"$TR"

n=0
hook() { # <transcript_path or ""> <env assignments...>
  local tp="$1"
  shift
  n=$((n + 1))
  : >"$LOG"
  jq -n --arg s "s-ctx-$n" --arg p "the prompt" --arg t "$tp" \
    '{session_id:$s, cwd:"/tmp", prompt:$p} + (if $t != "" then {transcript_path:$t} else {} end)' \
    | env KB_HOOK_FMT=kimi "$@" "$RECALL" >/dev/null 2>&1
}

echo "== kb-recall.sh KB_RECALL_CONTEXT =="

hook "$TR"
if grep -q '^recall ' "$LOG" && ! grep -q -- '--transcript' "$LOG"; then ok "flag off: no --transcript"; else bad "flag off leaked: $(cat "$LOG")"; fi

hook "$TR" KB_RECALL_CONTEXT=1
if grep -E '^recall the prompt ' "$LOG" | grep -q -- "--transcript $TR"; then ok "flag on + readable path: kb recall gets --transcript"; else bad "no --transcript on recall: $(cat "$LOG")"; fi

hook "$TMPROOT/missing.jsonl" KB_RECALL_CONTEXT=1
if grep -q '^recall ' "$LOG" && ! grep -q -- '--transcript' "$LOG"; then ok "flag on + missing file: no --transcript"; else bad "missing file not skipped: $(cat "$LOG")"; fi

hook "" KB_RECALL_CONTEXT=1
if grep -q '^recall ' "$LOG" && ! grep -q -- '--transcript' "$LOG"; then ok "flag on + no transcript_path: no --transcript"; else bad "absent path not skipped: $(cat "$LOG")"; fi

hook "$TR" KB_RECALL_CONTEXT=1 KB_TURN=1
if grep -E '^turn the prompt ' "$LOG" | grep -q -- "--transcript $TR"; then ok "KB_TURN=1: kb turn gets --transcript"; else bad "no --transcript on turn: $(cat "$LOG")"; fi

hook "$TR" KB_TURN=1
if grep -q '^turn ' "$LOG" && ! grep -q -- '--transcript' "$LOG"; then ok "KB_TURN=1 without the flag: no --transcript"; else bad "turn leaked: $(cat "$LOG")"; fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
