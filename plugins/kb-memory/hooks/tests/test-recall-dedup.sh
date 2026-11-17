#!/usr/bin/env bash
# test-recall-dedup.sh — SG-Q3: per-session dedup flags, the compact epoch
# marker, and the trivial-prompt skip (kb-recall.sh + kb-wake.sh).
#
#   1. Session id known: kb recall / kb turn get --dedup-session; no marker =
#      no --served-after.
#   2. recall-epoch-<sid> present: --served-after <its content>.
#   3. KB_RECALL_DEDUP=0 or no session id: no dedup flags.
#   4. kb-wake.sh: source=compact|clear writes recall-epoch-<sid>; startup
#      and resume do not.
#   5. Trivial prompts ("ok", "vai", "/compact", "sì") skip ONLY the recall
#      lane; a normal Italian / English sentence recalls; KB_RECALL_SKIP_TRIVIAL=0
#      and KB_RECALL_CONTEXT=1 disable the skip. KB_TURN=1 drops the recall lane
#      but keeps the others.
#   6. A CLI that rejects the dedup flags is retried without them.
#
# Fake `kb` on PATH logs its argv; `jq` is real.
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-recall-dedup.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RECALL="$HOOKS_DIR/kb-recall.sh"
WAKE="$HOOKS_DIR/kb-wake.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-recall-dedup-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT
# shellcheck source=lib-sandbox.sh
. "$SCRIPT_DIR/lib-sandbox.sh"
kb_test_sandbox "$TMPROOT" || exit 1

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

unset KB_HARNESS CLAUDE_PLUGIN_ROOT KB_SESSION_ID KB_RECALL_DEDUP KB_RECALL_SKIP_TRIVIAL KB_RECALL_CONTEXT KB_TURN

mkdir -p "$TMPROOT/bin"
export LOG="$TMPROOT/calls.log"
cat >"$TMPROOT/bin/kb" <<'EOF2'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$LOG"
if printf '%s' "$*" | grep -q -- '--dedup-session'; then
  [ -n "${FAKE_OLD_CLI:-}" ] && { echo "error: unexpected argument '--dedup-session' found" >&2; exit 2; }
  [ -n "${FAKE_EXIT2_QUIET:-}" ] && exit 2
  [ -n "${FAKE_EXIT1:-}" ] && { echo "boom" >&2; exit 1; }
  [ -n "${FAKE_TIMEOUT:-}" ] && exit 124
fi
case "$1" in
  turn) printf '%s' '{"text":"T","recalled":[]}'; exit 0 ;;
  recall) printf '%s' '{"hits":[{"title":"OLD","kb":"main","id":"abc123def456"}]}'; exit 0 ;;
  context) printf '%s' '{"scent":"no prior context"}'; exit 0 ;;
  slate) printf '%s' '{"text":"","head_seq":1}'; exit 0 ;;
esac
exit 0
EOF2
chmod +x "$TMPROOT/bin/kb"
export PATH="$TMPROOT/bin:$PATH"

n=0
hook() { # <prompt> <sid or ""> <env assignments...>
  local prompt="$1" sid="$2"
  shift 2
  n=$((n + 1))
  : >"$LOG"
  jq -n --arg s "$sid" --arg p "$prompt" \
    '{cwd:"/tmp", prompt:$p} + (if $s != "" then {session_id:$s} else {} end)' \
    | env KB_HOOK_FMT=kimi "$@" "$RECALL" >"$TMPROOT/out.txt" 2>/dev/null
}
wake() { # <sid> <source or ""> <env...>
  local sid="$1" src="$2"
  shift 2
  jq -n --arg s "$sid" --arg src "$src" \
    '{session_id:$s, cwd:"/tmp"} + (if $src != "" then {source:$src} else {} end)' \
    | env "$@" "$WAKE" >/dev/null 2>&1
}
recalled() { grep -q '^recall ' "$LOG"; }

SENT="please explain the deploy pipeline"
CACHE="$XDG_CACHE_HOME/kb"

echo "== dedup flags =="
hook "$SENT" "sid-a"
if grep '^recall ' "$LOG" | grep -q -- '--dedup-session' && ! grep -q -- '--served-after' "$LOG"; then ok "sid, no marker: --dedup-session only"; else bad "no marker: $(cat "$LOG")"; fi

mkdir -p "$CACHE"
printf '1700000123\n' >"$CACHE/recall-epoch-sid-b"
hook "$SENT" "sid-b"
if grep '^recall ' "$LOG" | grep -q -- '--dedup-session --served-after 1700000123'; then ok "marker present: --served-after <epoch>"; else bad "marker: $(cat "$LOG")"; fi

printf 'garbage\n' >"$CACHE/recall-epoch-sid-c"
hook "$SENT" "sid-c"
if grep '^recall ' "$LOG" | grep -q -- '--dedup-session' && ! grep -q -- '--served-after' "$LOG"; then ok "non-numeric marker ignored"; else bad "garbage marker: $(cat "$LOG")"; fi

hook "$SENT" "sid-a" KB_RECALL_DEDUP=0
if recalled && ! grep -q -- '--dedup-session' "$LOG"; then ok "KB_RECALL_DEDUP=0: no dedup flags"; else bad "kill switch: $(cat "$LOG")"; fi

hook "$SENT" ""
if recalled && ! grep -q -- '--dedup-session' "$LOG"; then ok "no session id: no dedup flags"; else bad "no sid: $(cat "$LOG")"; fi

hook "$SENT" "sid-b" KB_TURN=1
if grep '^turn ' "$LOG" | grep -q -- '--dedup-session --served-after 1700000123'; then ok "KB_TURN=1: kb turn gets the flags"; else bad "turn: $(cat "$LOG")"; fi

hook "$SENT" "sid-a" FAKE_OLD_CLI=1
if [ "$(grep -c '^recall ' "$LOG")" = "2" ] && grep -q 'OLD' "$TMPROOT/out.txt"; then ok "old CLI: retried without dedup, injection kept"; else bad "old CLI: $(cat "$LOG")"; fi

# F4: only a clap unknown-argument failure retries; stderr never leaks.
hook "$SENT" "sid-a" FAKE_OLD_CLI=1
if ! grep -q 'unexpected argument' "$TMPROOT/out.txt"; then ok "skew: stderr kept out of injected output"; else bad "stderr leaked"; fi
for mode in FAKE_EXIT1 FAKE_TIMEOUT FAKE_EXIT2_QUIET; do
  hook "$SENT" "sid-a" "$mode=1"
  if [ "$(grep -c '^recall ' "$LOG")" = "1" ]; then ok "$mode: no retry"; else bad "$mode retried: $(cat "$LOG")"; fi
done
hook "$SENT" "sid-a" FAKE_OLD_CLI=1 KB_TURN=1
if [ "$(grep -c '^turn ' "$LOG")" = "2" ] && ! grep '^turn ' "$LOG" | tail -n 1 | grep -q -- '--dedup-session'; then ok "turn: skew retried once without dedup"; else bad "turn skew: $(cat "$LOG")"; fi
hook "$SENT" "sid-a" FAKE_EXIT1=1 KB_TURN=1
if [ "$(grep -c '^turn ' "$LOG")" = "1" ]; then ok "turn: exit 1 no retry"; else bad "turn exit1: $(cat "$LOG")"; fi

echo "== sid sanitisation (F7) =="
mkdir -p "$CACHE"
printf '1700000999\n' >"$CACHE/recall-epoch-.."
hook "$SENT" ".."
if ! grep -q -- '--served-after' "$LOG"; then ok "sid '..': epoch not read"; else bad "dotdot: $(cat "$LOG")"; fi
printf '1700000999\n' >"$TMPROOT/evil"
hook "$SENT" "x/../../evil"
if ! grep -q -- '--served-after' "$LOG"; then ok "sid with slash: epoch not read"; else bad "slash: $(cat "$LOG")"; fi
hook "$SENT" "sid.b_1"
! grep -q -- '--served-after' "$LOG" && ok "sid with dot/underscore: allowed, no marker" || bad "dot sid"
rm -f "$CACHE/recall-epoch-.."
wake "x/y" "compact"; wake ".." "compact"; wake "a b" "clear"
if [ ! -e "$CACHE/recall-epoch-x" ] && [ ! -e "$CACHE/recall-epoch-.." ] && [ ! -e "$CACHE/recall-epoch-.. .tmp" ] && ! ls "$CACHE" | grep -q 'recall-epoch-a b'; then ok "wake: unsafe sids write no epoch"; else bad "wake wrote: $(ls -a "$CACHE")"; fi
wake "ok.sid_1" "compact"
[ -f "$CACHE/recall-epoch-ok.sid_1" ] && ok "wake: safe sid with ._ writes epoch" || bad "wake safe sid"

echo "== compact epoch marker =="
rm -f "$CACHE"/recall-epoch-w-*
wake "w-startup" "startup"
[ ! -f "$CACHE/recall-epoch-w-startup" ] && ok "startup: no marker" || bad "startup wrote a marker"
wake "w-resume" "resume"
[ ! -f "$CACHE/recall-epoch-w-resume" ] && ok "resume: no marker" || bad "resume wrote a marker"
wake "w-none" ""
[ ! -f "$CACHE/recall-epoch-w-none" ] && ok "no source: no marker" || bad "no-source wrote a marker"
before="$(date +%s)"
wake "w-compact" "compact"
v="$(cat "$CACHE/recall-epoch-w-compact" 2>/dev/null)"
case "$v" in '' | *[!0-9]*) bad "compact: marker missing or non-numeric ($v)" ;; *) [ "$v" -ge "$before" ] && ok "compact: marker holds the current unix time" || bad "compact epoch too old: $v" ;; esac
wake "w-clear" "clear"
[ -f "$CACHE/recall-epoch-w-clear" ] && ok "clear: marker written" || bad "clear wrote no marker"

echo "== trivial-prompt skip =="
for p in "ok" "vai" "/compact" "sì" "  /review the diff please" "ok grazie"; do
  hook "$p" "sid-t"
  if ! recalled; then ok "skips recall: '$p'"; else bad "recalled for trivial '$p'"; fi
done
for p in "/home/x/foo.rs why does this panic" "/tmp/a.log perché fallisce il test" "这个函数为什么会崩溃" "なぜこの関数はクラッシュしますか" "한국어 질문입니다"; do
  hook "$p" "sid-t"
  if recalled; then ok "recalls (path/CJK): '$p'"; else bad "skipped a real prompt '$p'"; fi
done
for p in "sì procedi" "ok" "/clear" "/foo:bar"; do
  hook "$p" "sid-t"
  if ! recalled; then ok "skips recall (it): '$p'"; else bad "recalled for trivial '$p'"; fi
done
for p in "come funziona il deploy della pipeline" "please explain the deploy pipeline" "fix bug 42"; do
  hook "$p" "sid-t"
  if recalled; then ok "recalls: '$p'"; else bad "skipped a real prompt '$p'"; fi
done
hook "ok" "sid-t" KB_RECALL_SKIP_TRIVIAL=0
recalled && ok "KB_RECALL_SKIP_TRIVIAL=0: recalls 'ok'" || bad "kill switch ignored"

TR="$TMPROOT/transcript.jsonl"
echo '{}' >"$TR"
n=$((n + 1)); : >"$LOG"
jq -n --arg t "$TR" '{session_id:"sid-t", cwd:"/tmp", prompt:"ok", transcript_path:$t}' \
  | env KB_HOOK_FMT=kimi KB_RECALL_CONTEXT=1 "$RECALL" >/dev/null 2>&1
recalled && ok "context transcript passed: no skip" || bad "skipped despite context transcript"

# Trivial prompt on turn 1: the scent lane still runs (first prompt of a new session).
hook "ok" "sid-first"
if ! recalled && grep -q '^context ' "$LOG"; then ok "trivial turn 1: scent still asked, recall skipped"; else bad "turn-1 lanes: $(cat "$LOG")"; fi

# KB_TURN=1: the recall lane is dropped from --lanes, context + slate stay.
hook "ok" "sid-first2" KB_TURN=1
lanes="$(grep '^turn ' "$LOG" | sed -n 's/.*--lanes \([^ ]*\).*/\1/p')"
if [ "$lanes" = "context,slate" ]; then ok "KB_TURN=1 trivial first turn: lanes=$lanes"; else bad "turn lanes: '$lanes' ($(cat "$LOG"))"; fi
hook "ok" "sid-first2" KB_TURN=1
if ! grep -q '^turn ' "$LOG" || ! grep '^turn ' "$LOG" | grep -q 'recall'; then ok "KB_TURN=1 trivial later turn: no recall lane"; else bad "later turn: $(cat "$LOG")"; fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
