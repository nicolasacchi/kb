#!/usr/bin/env bash
# test-wake-slate.sh — SL3: kb-wake.sh's slate HYBRID block (SessionStart).
#
# `kb slate open --hybrid --budget 2000 --session-id <sid> --cwd <cwd> --json`
# is appended after the memory index and the distill-pending block; `.text`
# (if non-empty) rides the injected context and `.head_seq` seeds
# `~/.cache/kb/slate-cursor-<sid>`. Every failure — empty text, a non-zero
# exit, malformed JSON — must leave output BYTE-IDENTICAL to the pre-SL3
# shape. `git` is stubbed to always fail for this whole file: slate slug
# derivation is entirely server-side (`--cwd`), so the hook must never shell
# out to git to resolve it (the pre-existing W0.3 commit-trailer marker
# already tolerates a missing/failing git and is unrelated to this feature).
#
# Fake `kb` on PATH (recall + slate open); `jq` is real.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-wake-slate.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WAKE="$HOOKS_DIR/kb-wake.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-wake-slate-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

mkdir -p "$TMPROOT/bin"
export PATH="$TMPROOT/bin:$PATH"

# git ALWAYS fails — proves the hook's slate call never depends on it.
cat >"$TMPROOT/bin/git" <<'EOF'
#!/usr/bin/env bash
exit 1
EOF
chmod +x "$TMPROOT/bin/git"

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

SID="wake-slate-sid-1"
CWD="/tmp/wake-slate-proj"

run_wake() {
  local cache="$1"
  export XDG_CACHE_HOME="$cache"
  mkdir -p "$cache"
  printf '%s' "{\"session_id\":\"$SID\",\"cwd\":\"$CWD\",\"source\":\"startup\"}" | "$WAKE"
}

echo "== kb-wake.sh slate HYBRID block test matrix =="
echo "tmp root: $TMPROOT"
echo

# --- 0. baseline: kb slate unreachable -> today's exact shape -------------
export SLATE_FAIL=1
base_out="$(run_wake "$TMPROOT/cache-base")"
unset SLATE_FAIL
case "$base_out" in
  *"kb memory is available"*"Recent memories:"*"- T1  [main]"*)
    ok "baseline still carries the protocol + memory index" ;;
  *) bad "baseline still carries the protocol + memory index (got: $base_out)" ;;
esac

# --- 1. non-empty slate text: appended AFTER the memory index -------------
export SLATE_JSON='{"text":"NOW v7  #1 [claude/aaa 2m]  doing the thing","head_seq":42}'
out1="$(run_wake "$TMPROOT/cache1")"
unset SLATE_JSON
case "$out1" in
  *"Recent memories:"*"NOW v7"*"doing the thing"*)
    ok "slate text appended after the memory index" ;;
  *) bad "slate text appended after the memory index (got: $out1)" ;;
esac
if [ -f "$TMPROOT/cache1/kb/slate-cursor-$SID" ]; then
  cur="$(cat "$TMPROOT/cache1/kb/slate-cursor-$SID")"
  [ "$cur" = "42" ] && ok "cursor file written with head_seq" \
    || bad "cursor file written with head_seq (got: $cur)"
else
  bad "cursor file written with head_seq (file missing)"
fi

# --- 2. empty .text, head_seq present: ctx byte-identical, cursor WRITTEN -
export SLATE_JSON='{"text":"","head_seq":50}'
out2="$(run_wake "$TMPROOT/cache2")"
unset SLATE_JSON
[ "$out2" = "$base_out" ] && ok "empty slate text -> byte-identical output" \
  || bad "empty slate text -> byte-identical output (differs)"
if [ -f "$TMPROOT/cache2/kb/slate-cursor-$SID" ]; then
  cur2="$(cat "$TMPROOT/cache2/kb/slate-cursor-$SID")"
  [ "$cur2" = "50" ] && ok "cursor still seeded on an empty-text success" \
    || bad "cursor still seeded on an empty-text success (got: $cur2)"
else
  bad "cursor still seeded on an empty-text success (file missing)"
fi

# --- 3. kb slate fails (non-zero exit): byte-identical, no cursor ---------
export SLATE_FAIL=1
out3="$(run_wake "$TMPROOT/cache3")"
unset SLATE_FAIL
[ "$out3" = "$base_out" ] && ok "kb slate failure -> byte-identical output" \
  || bad "kb slate failure -> byte-identical output (differs)"
[ ! -f "$TMPROOT/cache3/kb/slate-cursor-$SID" ] && ok "kb slate failure -> no cursor written" \
  || bad "kb slate failure -> no cursor written (file exists)"

# --- 4. malformed JSON: byte-identical, no cursor -------------------------
export SLATE_JSON='not-json{{{'
out4="$(run_wake "$TMPROOT/cache4")"
unset SLATE_JSON
[ "$out4" = "$base_out" ] && ok "malformed slate JSON -> byte-identical output" \
  || bad "malformed slate JSON -> byte-identical output (differs)"
[ ! -f "$TMPROOT/cache4/kb/slate-cursor-$SID" ] && ok "malformed slate JSON -> no cursor written" \
  || bad "malformed slate JSON -> no cursor written (file exists)"

# --- 5. worktree cwd forwarded VERBATIM; no git dependency ----------------
export KB_ARGV_SPY="$TMPROOT/argv-wake.txt"
export SLATE_JSON='{"text":"","head_seq":1}'
WORKTREE_CWD="/home/user/worktrees/slate-m1-linked"
export XDG_CACHE_HOME="$TMPROOT/cache5"
mkdir -p "$TMPROOT/cache5"
printf '%s' "{\"session_id\":\"$SID\",\"cwd\":\"$WORKTREE_CWD\",\"source\":\"startup\"}" | "$WAKE" >/dev/null
spy="$(cat "$KB_ARGV_SPY" 2>/dev/null)"
unset KB_ARGV_SPY SLATE_JSON
case "$spy" in
  *"--cwd $WORKTREE_CWD"*)
    ok "worktree cwd is forwarded verbatim to kb slate open (no bash-side slug)" ;;
  *) bad "worktree cwd is forwarded verbatim to kb slate open (got: $spy)" ;;
esac
case "$spy" in
  *"--session-id $SID"*) ok "kb slate open carries --session-id" ;;
  *) bad "kb slate open carries --session-id (got: $spy)" ;;
esac
case "$spy" in
  *"--hybrid"*"--budget 2000"*"--json"*) ok "kb slate open carries --hybrid --budget 2000 --json" ;;
  *) bad "kb slate open carries --hybrid --budget 2000 --json (got: $spy)" ;;
esac

# --- 6. hooks.json / settings.sample.json carry the compact matcher -------
if grep -q '"matcher": "startup|resume|clear|compact"' "$HOOKS_DIR/hooks.json"; then
  ok "hooks.json SessionStart matcher gains compact"
else
  bad "hooks.json SessionStart matcher gains compact"
fi
if grep -q '"matcher": "startup|resume|clear|compact"' "$HOOKS_DIR/settings.sample.json"; then
  ok "settings.sample.json SessionStart matcher gains compact"
else
  bad "settings.sample.json SessionStart matcher gains compact"
fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
