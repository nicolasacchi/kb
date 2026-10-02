#!/usr/bin/env bash
# test-hook-deadlines.sh — v0.44 H1: the Claude hooks' per-call caps must ADD UP
# to less than the hooks.json budget, and a slow daemon must degrade a hook
# (miss one lane) rather than kill it (lose every lane).
#
#   1. kb-wake.sh: a `kb recall` that never answers must still emit the
#      memory protocol (previously: no timeout -> the 15s harness kill -> no
#      SessionStart injection at all).
#   2. kb-recall.sh: a `kb recall` that never answers must NOT throw away the
#      turn-1 scent already computed (previously `|| exit 0`), and the whole
#      hook stays inside the shared KB_HOOK_BUDGET_SECS deadline.
#   3. kb-recall.sh sends the session id to `kb recall` through
#      KB_RECALL_SESSION alone — not through a stray `session=<sid>` word that
#      `env` would parse as a second (dead) environment assignment.
#   4. A memory summary containing a blank line is flattened to one `↳` line,
#      so the injected block has no blank line inside a hit (the capture
#      parser's walk ends at the first blank line).
#   5. The default shared budget is below every hooks.json timeout it runs under.
#
# Fake `kb` on PATH; `jq` and `timeout` are real.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-hook-deadlines.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RECALL="$HOOKS_DIR/kb-recall.sh"
WAKE="$HOOKS_DIR/kb-wake.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-hook-deadline-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

command -v timeout >/dev/null 2>&1 || { echo "timeout(1) missing — skipping"; exit 0; }

mkdir -p "$TMPROOT/bin"
export PATH="$TMPROOT/bin:$PATH"
export XDG_CACHE_HOME="$TMPROOT/cache"

# `exec sleep`, not `sleep`: timeout kills the process it started, and a
# forked sleep would keep the hook's $(...) pipe open until it finished.
cat >"$TMPROOT/bin/kb" <<'EOF2'
#!/usr/bin/env bash
case "$1" in
  context)
    printf '%s' '{"scent":"3 prior sessions · 2 open comments"}'
    exit 0
    ;;
  recall)
    if [ -n "${RECALL_ENV_DUMP:-}" ]; then env >"$RECALL_ENV_DUMP"; fi
    if [ "${RECALL_HANG:-0}" = "1" ]; then exec sleep 30; fi
    if [ -n "${RECALL_JSON:-}" ]; then
      printf '%s' "$RECALL_JSON"
    else
      printf '%s' '{"hits":[{"title":"T1","kb":"main","id":"abc123def456"}]}'
    fi
    exit 0
    ;;
esac
exit 0
EOF2
chmod +x "$TMPROOT/bin/kb"

now() { date +%s; }

echo "== hook deadline + recall block shape =="

# --- 1. wake: hung recall still emits the protocol -------------------------
t0="$(now)"
out="$(printf '%s' '{"session_id":"dl-wake","cwd":"/tmp"}' | RECALL_HANG=1 "$WAKE")"
el=$(( $(now) - t0 ))
ctx="$(printf '%s' "$out" | jq -r '.hookSpecificOutput.additionalContext // empty' 2>/dev/null)"
if [ "$el" -le 9 ]; then ok "wake returns within its cap with recall hung (${el}s)"; else bad "wake took ${el}s with recall hung"; fi
if [ -n "$ctx" ]; then ok "wake still emits the protocol when recall times out"; else bad "wake emitted nothing when recall timed out"; fi

# --- 2. recall hook: hung recall keeps the scent, stays in budget ----------
t0="$(now)"
out="$(printf '%s' '{"session_id":"dl-recall-1","cwd":"/tmp","prompt":"do the thing"}' \
  | KB_HOOK_BUDGET_SECS=2 RECALL_HANG=1 "$RECALL")"
el=$(( $(now) - t0 ))
ctx="$(printf '%s' "$out" | jq -r '.hookSpecificOutput.additionalContext // empty' 2>/dev/null)"
case "$ctx" in
  *"3 prior sessions"*) ok "a recall timeout keeps the already-computed turn-1 scent" ;;
  *) bad "a recall timeout dropped the scent (got: $ctx)" ;;
esac
if [ "$el" -le 4 ]; then ok "recall hook honours the shared budget (${el}s of 2)"; else bad "recall hook took ${el}s with a 2s budget"; fi

# --- 3. no stray session= word; KB_RECALL_SESSION carries the id -----------
dump="$TMPROOT/env.dump"
printf '%s' '{"session_id":"dl-recall-2","cwd":"/tmp","prompt":"again"}' \
  | RECALL_ENV_DUMP="$dump" "$RECALL" >/dev/null
if grep -qx 'KB_RECALL_SESSION=dl-recall-2' "$dump" 2>/dev/null; then ok "KB_RECALL_SESSION carries the session id"; else bad "KB_RECALL_SESSION missing"; fi
if grep -q '^session=' "$dump" 2>/dev/null; then bad "a stray session= assignment reached kb's environment"; else ok "no stray session= assignment"; fi

# --- 4. a blank line inside a summary is flattened -------------------------
json='{"hits":[{"title":"T1","kb":"main","id":"abc123def456","summary":"Fix:\n\nuse X"}]}'
for h in recall wake; do
  if [ "$h" = recall ]; then
    out="$(printf '%s' '{"cwd":"/tmp","prompt":"p"}' | RECALL_JSON="$json" "$RECALL")"
  else
    out="$(printf '%s' '{"cwd":"/tmp"}' | RECALL_JSON="$json" "$WAKE")"
  fi
  ctx="$(printf '%s' "$out" | jq -r '.hookSpecificOutput.additionalContext // empty' 2>/dev/null)"
  block="$(printf '%s\n' "$ctx" | sed -n '/^Relevant memories from kb/,/kb-recall\/1/p')"
  case "$block" in
    *"↳ Fix: use X"*) ok "$h: summary newlines flattened to one line" ;;
    *) bad "$h: summary not flattened (got: $block)" ;;
  esac
  if printf '%s\n' "$block" | grep -q '^$'; then bad "$h: blank line inside the injected hit"; else ok "$h: no blank line inside the injected hit"; fi
done

# --- 5. default budget below the hooks.json timeouts -----------------------
budget="$(grep -o 'KB_HOOK_BUDGET_SECS:-[0-9]*' "$RECALL" | head -n 1 | grep -o '[0-9]*$')"
min_to="$(jq -r '[.hooks.UserPromptSubmit[0].hooks[0].timeout, .hooks.SessionStart[0].hooks[0].timeout] | min' "$HOOKS_DIR/hooks.json")"
if [ -n "$budget" ] && [ "$budget" -lt "$min_to" ]; then ok "shared budget ${budget}s < hooks.json timeout ${min_to}s"; else bad "budget ${budget:-?} vs hooks.json ${min_to:-?}"; fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
