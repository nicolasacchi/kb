#!/usr/bin/env bash
# test-recall-kimi.sh — self-contained test matrix for kb-recall.sh's
# Kimi Code handling: the UserPromptSubmit payload's .prompt arrives as a
# content-parts ARRAY (verified live), and KB_HOOK_FMT=kimi switches the
# output from Claude's hookSpecificOutput envelope to plain stdout text.
# Claude/Codex (string .prompt, no KB_HOOK_FMT) must stay byte-identical.
# Fake `kb` on PATH (only `kb recall` is exercised); `jq` is real.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-recall-kimi.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RECALL="$HOOKS_DIR/kb-recall.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-recall-kimi-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

mkdir -p "$TMPROOT/bin"
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "recall" ]; then
  echo '{"hits":[{"title":"T1","kb":"main","id":"abc123def456","summary":"a memory"}]}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
export PATH="$TMPROOT/bin:$PATH"
export XDG_CACHE_HOME="$TMPROOT/cache"

echo "== kb-recall.sh kimi test matrix =="
echo "tmp root: $TMPROOT"
echo

# --- 1. array-shaped .prompt (live kimi shape) + KB_HOOK_FMT=kimi -------
out="$(printf '%s' '{"hook_event_name":"UserPromptSubmit","session_id":"s-kimi","cwd":"/tmp","prompt":[{"type":"text","text":"how do widgets work?"}],"is_steer":false}' \
  | KB_HOOK_FMT=kimi "$RECALL")"
case "$out" in
  *"Relevant memories from kb"*"- T1  [main]"*) ok "array .prompt + KB_HOOK_FMT=kimi -> plain-text block" ;;
  *) bad "array .prompt + KB_HOOK_FMT=kimi -> plain-text block (got: $out)" ;;
esac
if printf '%s' "$out" | jq -e . >/dev/null 2>&1; then
  bad "kimi-mode output is NOT a JSON envelope"
else
  ok "kimi-mode output is NOT a JSON envelope"
fi
# MR1 layout v2 — the bare-stdout branch carries the same v2 block as the
# envelope branch: no `(id …)` parenthetical, `pos=` in the marker. The
# layout is orthogonal to KB_HOOK_FMT (test-recall-layout.sh asserts the
# converse direction).
case "$out" in
  *"(id "*) bad "kimi-mode block is layout v2 (no parenthetical)" ;;
  *) ok "kimi-mode block is layout v2 (no parenthetical)" ;;
esac
case "$out" in
  *"<!--kb-recall/1 kb=main id=abc123def456 pos=1-->"*)
    ok "kimi-mode block carries the pos= marker" ;;
  *) bad "kimi-mode block carries the pos= marker (got: $out)" ;;
esac

# --- 2. string .prompt, no KB_HOOK_FMT -> Claude envelope (unchanged) ---
out2="$(printf '%s' '{"session_id":"s-claude","cwd":"/tmp","prompt":"how do widgets work?"}' | "$RECALL")"
if printf '%s' "$out2" | jq -e '.hookSpecificOutput.hookEventName == "UserPromptSubmit"
     and (.hookSpecificOutput.additionalContext | contains("- T1  [main]"))' >/dev/null 2>&1; then
  ok "string .prompt default -> Claude hookSpecificOutput envelope"
else
  bad "string .prompt default -> Claude hookSpecificOutput envelope (got: $out2)"
fi

# --- 3. .input fallback when .prompt is absent ---------------------------
out3="$(printf '%s' '{"session_id":"s-in","cwd":"/tmp","input":"fallback question"}' | "$RECALL")"
if printf '%s' "$out3" | jq -e '.hookSpecificOutput.additionalContext' >/dev/null 2>&1; then
  ok ".input fallback accepted when .prompt absent"
else
  bad ".input fallback accepted when .prompt absent (got: $out3)"
fi

# --- 4. no prompt at all -> silent, exit 0 -------------------------------
out4="$(printf '%s' '{"session_id":"s-none","cwd":"/tmp"}' | "$RECALL")"
if [ -z "$out4" ]; then
  ok "no prompt -> silent"
else
  bad "no prompt -> silent (got: $out4)"
fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
