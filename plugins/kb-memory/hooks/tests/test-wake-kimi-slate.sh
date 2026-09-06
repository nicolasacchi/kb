#!/usr/bin/env bash
# test-wake-kimi-slate.sh — SL3: kb-wake-kimi.sh's slate HYBRID block (Kimi
# Code's bare-stdout UserPromptSubmit wake lane). Same contract as
# test-wake-slate.sh's kb-wake.sh coverage, adjusted for the once-per-session
# marker gate and plain-stdout (no hookSpecificOutput envelope) shape.
#
# Fake `kb` on PATH (recall + slate open); `jq` is real.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-wake-kimi-slate.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WAKE="$HOOKS_DIR/kb-wake-kimi.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-wake-kimi-slate-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

mkdir -p "$TMPROOT/bin"
export PATH="$TMPROOT/bin:$PATH"

cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
case "$1" in
  recall)
    printf '%s' '{"hits":[{"title":"T1","kb":"main","summary":"a memory"}]}'
    exit 0
    ;;
  slate)
    if [ -n "${KB_ARGV_SPY:-}" ]; then
      { printf 'kb'; printf ' %s' "$@"; printf '\n'; } >>"$KB_ARGV_SPY"
    fi
    [ "${SLATE_FAIL:-0}" = "1" ] && exit 1
    body="${SLATE_JSON:-}"
    [ -n "$body" ] || body='{"text":"","head_seq":1}'
    printf '%s' "$body"
    exit 0
    ;;
esac
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"

CWD="/tmp/wake-kimi-slate-proj"

run_wake() {
  local sid="$1" cache="$2"
  export XDG_CACHE_HOME="$cache"
  mkdir -p "$cache"
  printf '%s' "{\"session_id\":\"$sid\",\"cwd\":\"$CWD\",\"hook_event_name\":\"UserPromptSubmit\"}" | "$WAKE"
}

echo "== kb-wake-kimi.sh slate HYBRID block test matrix =="
echo "tmp root: $TMPROOT"
echo

# --- 0. baseline: kb slate unreachable -> today's exact bare-stdout shape -
export SLATE_FAIL=1
base_out="$(run_wake "wk-sid-0" "$TMPROOT/cache-base")"
unset SLATE_FAIL
case "$base_out" in
  *"kb memory is available"*"Recent memories:"*"- T1  [main]"*)
    ok "baseline still carries the protocol + memory index" ;;
  *) bad "baseline still carries the protocol + memory index (got: $base_out)" ;;
esac
case "$base_out" in
  *"hookSpecificOutput"*) bad "baseline must stay bare stdout (no envelope)" ;;
  *) ok "baseline stays bare stdout (no envelope)" ;;
esac

# --- 1. non-empty slate text: appended AFTER the memory index -------------
export SLATE_JSON='{"text":"NOW v7  #1 [claude/aaa 2m]  doing the thing","head_seq":42}'
out1="$(run_wake "wk-sid-1" "$TMPROOT/cache1")"
unset SLATE_JSON
case "$out1" in
  *"Recent memories:"*"NOW v7"*"doing the thing"*)
    ok "slate text appended after the memory index" ;;
  *) bad "slate text appended after the memory index (got: $out1)" ;;
esac
if [ -f "$TMPROOT/cache1/kb/slate-cursor-wk-sid-1" ]; then
  cur="$(cat "$TMPROOT/cache1/kb/slate-cursor-wk-sid-1")"
  [ "$cur" = "42" ] && ok "cursor file written with head_seq" \
    || bad "cursor file written with head_seq (got: $cur)"
else
  bad "cursor file written with head_seq (file missing)"
fi
[ -f "$TMPROOT/cache1/kb/waked-kimi-wk-sid-1" ] && ok "once-per-session marker created" \
  || bad "once-per-session marker created (missing)"

# --- 2. empty .text, head_seq present: byte-identical, cursor WRITTEN -----
export SLATE_JSON='{"text":"","head_seq":50}'
out2="$(run_wake "wk-sid-2" "$TMPROOT/cache2")"
unset SLATE_JSON
[ "$out2" = "$base_out" ] && ok "empty slate text -> byte-identical output" \
  || bad "empty slate text -> byte-identical output (differs)"
if [ -f "$TMPROOT/cache2/kb/slate-cursor-wk-sid-2" ]; then
  cur2="$(cat "$TMPROOT/cache2/kb/slate-cursor-wk-sid-2")"
  [ "$cur2" = "50" ] && ok "cursor still seeded on an empty-text success" \
    || bad "cursor still seeded on an empty-text success (got: $cur2)"
else
  bad "cursor still seeded on an empty-text success (file missing)"
fi

# --- 3. kb slate fails (non-zero exit): byte-identical, no cursor ---------
export SLATE_FAIL=1
out3="$(run_wake "wk-sid-3" "$TMPROOT/cache3")"
unset SLATE_FAIL
[ "$out3" = "$base_out" ] && ok "kb slate failure -> byte-identical output" \
  || bad "kb slate failure -> byte-identical output (differs)"
[ ! -f "$TMPROOT/cache3/kb/slate-cursor-wk-sid-3" ] && ok "kb slate failure -> no cursor written" \
  || bad "kb slate failure -> no cursor written (file exists)"

# --- 4. second prompt of the SAME session id stays silent (marker gate) --
out4="$(run_wake "wk-sid-1" "$TMPROOT/cache1")"
[ -z "$out4" ] && ok "second prompt of the same session is silent (marker gate)" \
  || bad "second prompt of the same session is silent (got: $out4)"

# --- 5. worktree cwd forwarded VERBATIM ------------------------------------
export KB_ARGV_SPY="$TMPROOT/argv-wake.txt"
export SLATE_JSON='{"text":"","head_seq":1}'
WORKTREE_CWD="/home/user/worktrees/slate-m1-linked"
export XDG_CACHE_HOME="$TMPROOT/cache5"
mkdir -p "$TMPROOT/cache5"
printf '%s' "{\"session_id\":\"wk-sid-5\",\"cwd\":\"$WORKTREE_CWD\",\"hook_event_name\":\"UserPromptSubmit\"}" | "$WAKE" >/dev/null
spy="$(cat "$KB_ARGV_SPY" 2>/dev/null)"
unset KB_ARGV_SPY SLATE_JSON
case "$spy" in
  *"--cwd $WORKTREE_CWD"*)
    ok "worktree cwd is forwarded verbatim to kb slate open" ;;
  *) bad "worktree cwd is forwarded verbatim to kb slate open (got: $spy)" ;;
esac

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
