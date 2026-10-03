#!/usr/bin/env bash
# test-recall-turn.sh — v0.44 F6: kb-recall.sh's KB_TURN=1 path (ONE `kb turn`
# call instead of `kb context` + `kb recall`).
#
#   1. KB_TURN=1 injects exactly the daemon's `text`, and never spawns
#      `kb recall` / `kb context` when the turn call answered.
#   2. A degraded lane is NAMED in one trailing line, not dropped silently.
#   3. Turn 1 asks for lanes recall,context; later turns ask for recall only.
#   4. ANY failure of `kb turn` (older daemon, unreachable, bad JSON) falls
#      back to the old two-call path, whose output is unchanged.
#   5. Without KB_TURN=1, `kb turn` is never called (the flag is the A/B gate),
#      and a non-v2 KB_RECALL_LAYOUT takes the old path (the route speaks v2).
#
# Fake `kb` on PATH logs its argv; `jq` is real.
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-recall-turn.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RECALL="$HOOKS_DIR/kb-recall.sh"
V2="$SCRIPT_DIR/fixtures/recall-layout-v2.txt"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-recall-turn-test.XXXXXX")"
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
  turn)
    [ "${TURN_FAIL:-0}" = "1" ] && exit 1
    [ -n "${TURN_JSON:-}" ] && { printf '%s' "$TURN_JSON"; exit 0; }
    exit 1
    ;;
  recall)
    printf '%s' '{"hits":[{"title":"OLD","kb":"main","id":"abc123def456"}]}'
    exit 0
    ;;
  context)
    printf '%s' '{"scent":"2 prior sessions"}'
    exit 0
    ;;
esac
exit 0
EOF2
chmod +x "$TMPROOT/bin/kb"
export PATH="$TMPROOT/bin:$PATH"
export XDG_CACHE_HOME="$TMPROOT/cache"

n=0
hook() { # <extra env assignments...> ; prints the injected block (kimi shape)
  n=$((n + 1))
  : >"$LOG"
  printf '%s' "{\"session_id\":\"s-turn-$n\",\"cwd\":\"/tmp\",\"prompt\":\"q\"}" \
    | env KB_HOOK_FMT=kimi "$@" "$RECALL" 2>/dev/null
}
gold="$(cat "$V2")"
text_json="$(jq -n --arg t "$gold" '{text:$t, recalled:[], })' 2>/dev/null || true)"
text_json="$(jq -n --arg t "$gold" '{text:$t, recalled:[]}')"

echo "== kb-recall.sh KB_TURN=1 =="

# --- 1. injects the daemon text; the two old calls are not made ----------
out="$(hook KB_TURN=1 TURN_JSON="$text_json")"
if [ "$out" = "$gold" ]; then ok "KB_TURN=1 injects the daemon's text byte-for-byte"; else bad "text not injected verbatim"; printf '%s\n' "$out" | head -5; fi
if grep -q '^turn ' "$LOG" && ! grep -qE '^(recall|context) ' "$LOG"; then ok "only kb turn ran (no kb recall / kb context)"; else bad "unexpected calls: $(tr '\n' '|' <"$LOG")"; fi

# --- 3. lanes: first turn recall,context ; later turn recall -------------
if grep -q -- '--lanes recall,context' "$LOG"; then ok "turn 1 asks for lanes recall,context"; else bad "turn 1 lanes wrong: $(cat "$LOG")"; fi
if grep -q -- '--deadline-ms' "$LOG"; then ok "a deadline is passed to the daemon"; else bad "no --deadline-ms"; fi
n=$((n + 1)); : >"$LOG"
printf '%s' '{"session_id":"s-turn-repeat","cwd":"/tmp","prompt":"q"}' | KB_HOOK_FMT=kimi KB_TURN=1 TURN_JSON="$text_json" "$RECALL" >/dev/null 2>&1
printf '%s' '{"session_id":"s-turn-repeat","cwd":"/tmp","prompt":"q2"}' | KB_HOOK_FMT=kimi KB_TURN=1 TURN_JSON="$text_json" "$RECALL" >/dev/null 2>&1
if [ "$(grep -c -- '--lanes recall$' "$LOG" || true)" -ge 1 ] || grep -qE -- '--lanes recall --' "$LOG"; then ok "turn 2+ asks for lanes recall only"; else bad "later-turn lanes wrong: $(cat "$LOG")"; fi

# --- 2. a degraded lane is named -----------------------------------------
deg_json="$(jq -n --arg t "$gold" '{text:$t, recalled:[], degraded:[{kb:"turn",lane:"recall",error_class:"timeout"}], degraded_note:"kb: recall skipped (timeout)"}')"
out="$(hook KB_TURN=1 TURN_JSON="$deg_json")"
last="$(printf '%s\n' "$out" | tail -n 1)"
if [ "$last" = "kb: recall skipped (timeout)" ]; then ok "a degraded lane is named in one trailing line"; else bad "degraded line missing (last: $last)"; fi
if [ "$(printf '%s\n' "$out" | head -n -2)" = "$gold" ]; then ok "the named line is appended AFTER the untouched block"; else bad "block altered by the degraded line"; fi
empty_deg="$(jq -n '{text:"", recalled:[], degraded_note:"kb: recall skipped (timeout)"}')"
out="$(hook KB_TURN=1 TURN_JSON="$empty_deg")"
if [ "$out" = "kb: recall skipped (timeout)" ]; then ok "an all-lanes-failed turn still says so (not silence)"; else bad "empty-turn note lost: $out"; fi

# --- 4. failure falls back to the old path --------------------------------
out="$(hook KB_TURN=1 TURN_FAIL=1)"
case "$out" in
  *"OLD"*) ok "kb turn failing falls back to kb recall" ;;
  *) bad "no fallback (got: $out)" ;;
esac
if grep -q '^recall ' "$LOG"; then ok "fallback spawned kb recall"; else bad "fallback never called recall"; fi
out="$(hook KB_TURN=1 TURN_JSON='not json')"
case "$out" in
  *"OLD"*) ok "bad JSON from kb turn falls back" ;;
  *) bad "bad JSON not handled (got: $out)" ;;
esac

# --- 5. the flag gates it; non-v2 layouts keep the old path ---------------
out="$(hook TURN_JSON="$text_json")"
if ! grep -q '^turn ' "$LOG"; then ok "without KB_TURN=1, kb turn is never called"; else bad "kb turn called without the flag"; fi
case "$out" in *"OLD"*) ok "default path output unchanged" ;; *) bad "default path changed: $out" ;; esac
out="$(hook KB_TURN=1 KB_RECALL_LAYOUT=v1 TURN_JSON="$text_json")"
if ! grep -q '^turn ' "$LOG"; then ok "KB_RECALL_LAYOUT=v1 bypasses kb turn"; else bad "kb turn used under layout v1"; fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
