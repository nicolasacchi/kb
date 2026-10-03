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
WAKE_KIMI="$HOOKS_DIR/kb-wake-kimi.sh"

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

# --- 6. kimi wake twin: same cap + same flatten ----------------------------
t0="$(now)"
out="$(printf '%s' '{"session_id":"dl-kimi-1","cwd":"/tmp"}' | RECALL_HANG=1 "$WAKE_KIMI")"
el=$(( $(now) - t0 ))
if [ "$el" -le 9 ]; then ok "kimi wake returns within its cap with recall hung (${el}s)"; else bad "kimi wake took ${el}s with recall hung"; fi
if [ -n "$out" ]; then ok "kimi wake still emits the protocol when recall times out"; else bad "kimi wake emitted nothing when recall timed out"; fi
out="$(printf '%s' '{"session_id":"dl-kimi-2","cwd":"/tmp"}' | RECALL_JSON="$json" "$WAKE_KIMI")"
block="$(printf '%s\n' "$out" | sed -n '/^Relevant memories from kb/,/kb-recall\/1/p')"
case "$block" in
  *"↳ Fix: use X"*) ok "kimi wake: summary newlines flattened to one line" ;;
  *) bad "kimi wake: summary not flattened (got: $block)" ;;
esac
if printf '%s\n' "$block" | grep -q '^$'; then bad "kimi wake: blank line inside the injected hit"; else ok "kimi wake: no blank line inside the injected hit"; fi

# --- 5. default budget below the hooks.json timeouts -----------------------
budget="$(grep -o 'KB_HOOK_BUDGET_SECS:-[0-9]*' "$HOOKS_DIR/kb-hook-lib.sh" | head -n 1 | grep -o '[0-9]*$')"
min_to="$(jq -r '[.hooks.UserPromptSubmit[0].hooks[0].timeout, .hooks.SessionStart[0].hooks[0].timeout] | min' "$HOOKS_DIR/hooks.json")"
if [ -n "$budget" ] && [ "$budget" -lt "$min_to" ]; then ok "shared budget ${budget}s < hooks.json timeout ${min_to}s"; else bad "budget ${budget:-?} vs hooks.json ${min_to:-?}"; fi

# --- 7. worst case: EVERY lane hangs, wall time stays inside the budget ---
# Measured on a millisecond clock (the hooks' own helper), so the assertion
# does not rest on `date +%s` granularity: with a 1s clock a hook that
# overran by 900ms could still read as "within 1s".
# shellcheck disable=SC1091
. "$HOOKS_DIR/kb-hook-lib.sh"
cat >"$TMPROOT/bin/kb" <<'EOF3'
#!/usr/bin/env bash
exec sleep 30
EOF3
chmod +x "$TMPROOT/bin/kb"
t0="$(hook_now_ms)"
printf '%s' '{"session_id":"dl-worst-1","cwd":"/tmp","prompt":"p"}' \
  | KB_HOOK_BUDGET_SECS=2 KB_TURN=1 "$RECALL" >/dev/null
el_ms=$(( $(hook_now_ms) - t0 ))
# budget 2000ms + one process start / jq pass of slack.
if [ "$el_ms" -le 4000 ]; then ok "all lanes hung (KB_TURN=1, turn 1): ${el_ms}ms <= 2000ms budget + slack(2s)"; else bad "all lanes hung took ${el_ms}ms against a 2000ms budget"; fi
t0="$(hook_now_ms)"
printf '%s' '{"session_id":"dl-worst-2","cwd":"/tmp","prompt":"p"}' \
  | KB_HOOK_BUDGET_SECS=2 "$RECALL" >/dev/null
el_ms=$(( $(hook_now_ms) - t0 ))
if [ "$el_ms" -le 4000 ]; then ok "all lanes hung (old path, turn 1): ${el_ms}ms <= 2000ms budget + slack(2s)"; else bad "old path took ${el_ms}ms against a 2000ms budget"; fi
# And the DEFAULT budget plus the worst per-call overshoot is under every
# hooks.json timeout: the budget is the only thing the caps add up against.
if [ "$((budget * 1000 + 800))" -lt "$((min_to * 1000))" ]; then ok "default budget ${budget}s + 0.8s slack < hooks.json ${min_to}s"; else bad "default budget leaves no slack under ${min_to}s"; fi

# --- 8. the shared lib's clock is sub-second and run_to honours it -------
a="$(hook_now_ms)"; sleep 0.2; b="$(hook_now_ms)"
if [ $((b - a)) -ge 150 ] && [ $((b - a)) -lt 1000 ]; then ok "hook_now_ms resolves sub-second ($((b - a))ms for a 200ms sleep)"; else bad "hook_now_ms not sub-second: $((b - a))ms"; fi
KB_HOOK_BUDGET_SECS=1 hook_deadline_init
t0="$(hook_now_ms)"
run_to 30 sleep 30
rc=$?
el_ms=$(( $(hook_now_ms) - t0 ))
if [ "$rc" -eq 124 ] && [ "$el_ms" -le 1500 ]; then ok "run_to clips a 30s cap to the 1s budget (${el_ms}ms, rc 124)"; else bad "run_to rc=$rc after ${el_ms}ms"; fi
KB_HOOK_BUDGET_SECS=5 hook_deadline_init
run_to 30 true; rc=$?
KB_HOOK_BUDGET_SECS=0 hook_deadline_init
run_to 30 true; rc0=$?
if [ "$rc" -eq 0 ] && [ "$rc0" -eq 124 ]; then ok "run_to skips a call once the budget is spent (rc 124), runs one within it"; else bad "run_to budget skip: rc=$rc rc0=$rc0"; fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
