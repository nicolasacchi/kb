#!/usr/bin/env bash
# test-recall-slate.sh — SL3: kb-recall.sh's slate per-prompt DELTA.
#
# Cursor-gated: an absent `~/.cache/kb/slate-cursor-<sid>` means the wake
# hook never opened this session's slate, so the delta stays silent and
# `kb slate delta` must not even be invoked (the wake hook seeds the
# cursor; without one this hook SEEDS via open --hybrid). When a cursor exists, `timeout 2 kb slate
# delta --since <cursor> --session-id <sid> --cwd <cwd> --budget 1500
# --json` runs; a non-empty `.text` is appended (blank-line separated)
# after the ordinary recall block AND advances the cursor to `.head_seq`
# — an empty `.text` leaves the output untouched but still advances the
# cursor to `.head_seq` (same unconditional seed as the wake block's cursor
# write: nothing moved the needle, so nothing should look like it did).
# `git` is stubbed to always fail: slate slug derivation is entirely
# server-side (`--cwd`), so this hook must never shell out to git for it.
#
# Fake `kb` on PATH (context stub always "no prior context" so the CT-D1
# scent branch never interferes with these assertions; recall; slate
# delta); `jq` is real.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-recall-slate.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RECALL="$HOOKS_DIR/kb-recall.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-recall-slate-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

mkdir -p "$TMPROOT/bin"
export PATH="$TMPROOT/bin:$PATH"

# git ALWAYS fails — proves the slate delta call never depends on it.
cat >"$TMPROOT/bin/git" <<'EOF'
#!/usr/bin/env bash
exit 1
EOF
chmod +x "$TMPROOT/bin/git"

cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
case "$1" in
  context)
    printf '%s' '{"scent":"no prior context"}'
    exit 0
    ;;
  recall)
    r="${RECALL_JSON:-}"
    [ -n "$r" ] || r='{"hits":[{"title":"T1","kb":"main","id":"abc123def456"}]}'
    printf '%s' "$r"
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

CWD="/tmp/recall-slate-proj"

echo "== kb-recall.sh slate DELTA test matrix =="
echo "tmp root: $TMPROOT"
echo

# --- 0. reference: plain recall, no cursor anywhere ------------------------
export XDG_CACHE_HOME="$TMPROOT/cache-base"
mkdir -p "$TMPROOT/cache-base"
base_out="$(printf '%s' "{\"session_id\":\"rc-sid-base\",\"cwd\":\"$CWD\",\"prompt\":\"hello\"}" | "$RECALL")"
case "$base_out" in
  *"Relevant memories from kb"*"- T1  [main]"*) ok "reference recall block renders" ;;
  *) bad "reference recall block renders (got: $base_out)" ;;
esac

# --- 1. NO cursor file -> SEED: open --hybrid, block appended, cursor written
# (Codex has no session-start lane; this hook is how it first sees the slate.)
export XDG_CACHE_HOME="$TMPROOT/cache0"
mkdir -p "$TMPROOT/cache0"
export KB_ARGV_SPY="$TMPROOT/argv0.txt"
export SLATE_JSON='{"text":"NOW v7  #1  seeded hybrid block","head_seq":7}'
out0="$(printf '%s' "{\"session_id\":\"rc-sid-0\",\"cwd\":\"$CWD\",\"prompt\":\"hello\"}" | "$RECALL")"
unset KB_ARGV_SPY SLATE_JSON
case "$out0" in
  *"Relevant memories from kb"*"seeded hybrid block"*) ok "no cursor -> hybrid block appended after the recall block" ;;
  *) bad "no cursor -> hybrid block appended (got: $out0)" ;;
esac
grep -q -- 'kb slate open --hybrid --budget 2000' "$TMPROOT/argv0.txt" 2>/dev/null \
  && ok "no cursor -> kb slate open --hybrid invoked (not delta)" \
  || bad "no cursor -> kb slate open --hybrid invoked (spy: $(cat "$TMPROOT/argv0.txt" 2>/dev/null))"
cur0="$(cat "$TMPROOT/cache0/kb/slate-cursor-rc-sid-0" 2>/dev/null)"
[ "$cur0" = "7" ] && ok "no cursor -> cursor seeded with head_seq" \
  || bad "no cursor -> cursor seeded with head_seq (got: $cur0)"

# --- 1b. NO cursor, kb slate FAILS -> byte-identical, no cursor file --------
export XDG_CACHE_HOME="$TMPROOT/cache0b"
mkdir -p "$TMPROOT/cache0b"
export SLATE_FAIL=1
out0b="$(printf '%s' "{\"session_id\":\"rc-sid-0b\",\"cwd\":\"$CWD\",\"prompt\":\"hello\"}" | "$RECALL")"
unset SLATE_FAIL
[ "$out0b" = "$base_out" ] && ok "no cursor + kb failure -> byte-identical output" \
  || bad "no cursor + kb failure -> byte-identical output (differs: $out0b)"
[ ! -e "$TMPROOT/cache0b/kb/slate-cursor-rc-sid-0b" ] && ok "no cursor + kb failure -> no cursor file written" \
  || bad "no cursor + kb failure -> no cursor file written"

# --- 2. cursor exists, non-empty delta text: appended + cursor advances ----
export XDG_CACHE_HOME="$TMPROOT/cache1"
mkdir -p "$TMPROOT/cache1/kb"
printf '10' >"$TMPROOT/cache1/kb/slate-cursor-rc-sid-1"
export SLATE_JSON='{"text":"NOW v7  #2  fresh delta line","head_seq":15}'
out1="$(printf '%s' "{\"session_id\":\"rc-sid-1\",\"cwd\":\"$CWD\",\"prompt\":\"hello\"}" | "$RECALL")"
unset SLATE_JSON
case "$out1" in
  *"Relevant memories from kb"*"NOW v7"*"fresh delta line"*)
    ok "delta text appended after the recall block" ;;
  *) bad "delta text appended after the recall block (got: $out1)" ;;
esac
cur1="$(cat "$TMPROOT/cache1/kb/slate-cursor-rc-sid-1" 2>/dev/null)"
[ "$cur1" = "15" ] && ok "cursor advanced to the new head_seq" \
  || bad "cursor advanced to the new head_seq (got: $cur1)"

# --- 3. cursor exists, EMPTY delta text: byte-identical, cursor ADVANCES ---
export XDG_CACHE_HOME="$TMPROOT/cache2"
mkdir -p "$TMPROOT/cache2/kb"
printf '20' >"$TMPROOT/cache2/kb/slate-cursor-rc-sid-2"
export SLATE_JSON='{"text":"","head_seq":33}'
out2="$(printf '%s' "{\"session_id\":\"rc-sid-2\",\"cwd\":\"$CWD\",\"prompt\":\"hello\"}" | "$RECALL")"
unset SLATE_JSON
[ "$out2" = "$base_out" ] && ok "empty delta text -> byte-identical output" \
  || bad "empty delta text -> byte-identical output (differs: $out2)"
cur2="$(cat "$TMPROOT/cache2/kb/slate-cursor-rc-sid-2" 2>/dev/null)"
[ "$cur2" = "33" ] && ok "cursor advanced to head_seq even when delta text is empty" \
  || bad "cursor advanced to head_seq even when delta text is empty (got: $cur2)"

# --- 4. kb slate delta FAILS: byte-identical, cursor UNCHANGED -------------
export XDG_CACHE_HOME="$TMPROOT/cache3"
mkdir -p "$TMPROOT/cache3/kb"
printf '20' >"$TMPROOT/cache3/kb/slate-cursor-rc-sid-3"
export SLATE_FAIL=1
out3="$(printf '%s' "{\"session_id\":\"rc-sid-3\",\"cwd\":\"$CWD\",\"prompt\":\"hello\"}" | "$RECALL")"
unset SLATE_FAIL
[ "$out3" = "$base_out" ] && ok "kb slate delta failure -> byte-identical output" \
  || bad "kb slate delta failure -> byte-identical output (differs: $out3)"
cur3="$(cat "$TMPROOT/cache3/kb/slate-cursor-rc-sid-3" 2>/dev/null)"
[ "$cur3" = "20" ] && ok "cursor unchanged on delta failure" \
  || bad "cursor unchanged on delta failure (got: $cur3)"

# --- 5. malformed JSON from delta: byte-identical, cursor UNCHANGED --------
export XDG_CACHE_HOME="$TMPROOT/cache4"
mkdir -p "$TMPROOT/cache4/kb"
printf '20' >"$TMPROOT/cache4/kb/slate-cursor-rc-sid-4"
export SLATE_JSON='not-json{{{'
out4="$(printf '%s' "{\"session_id\":\"rc-sid-4\",\"cwd\":\"$CWD\",\"prompt\":\"hello\"}" | "$RECALL")"
unset SLATE_JSON
[ "$out4" = "$base_out" ] && ok "malformed delta JSON -> byte-identical output" \
  || bad "malformed delta JSON -> byte-identical output (differs: $out4)"
cur4="$(cat "$TMPROOT/cache4/kb/slate-cursor-rc-sid-4" 2>/dev/null)"
[ "$cur4" = "20" ] && ok "cursor unchanged on malformed delta JSON" \
  || bad "cursor unchanged on malformed delta JSON (got: $cur4)"

# --- 6. kimi bare-stdout branch carries the delta block --------------------
export XDG_CACHE_HOME="$TMPROOT/cache5"
mkdir -p "$TMPROOT/cache5/kb"
printf '10' >"$TMPROOT/cache5/kb/slate-cursor-rc-sid-5"
export SLATE_JSON='{"text":"NOW v7  #9  kimi delta line","head_seq":11}'
out5="$(printf '%s' "{\"session_id\":\"rc-sid-5\",\"cwd\":\"$CWD\",\"prompt\":[{\"type\":\"text\",\"text\":\"hi\"}]}" \
  | KB_HOOK_FMT=kimi "$RECALL")"
unset SLATE_JSON
case "$out5" in
  *"hookSpecificOutput"*) bad "kimi format must stay bare (got: $out5)" ;;
  *) ok "kimi format stays bare (no envelope)" ;;
esac
case "$out5" in
  *"kimi delta line"*) ok "kimi bare-stdout branch carries the slate delta block" ;;
  *) bad "kimi bare-stdout branch carries the slate delta block (got: $out5)" ;;
esac

# --- 7. worktree cwd forwarded VERBATIM; --since reads the cursor file -----
export XDG_CACHE_HOME="$TMPROOT/cache6"
mkdir -p "$TMPROOT/cache6/kb"
printf '10' >"$TMPROOT/cache6/kb/slate-cursor-rc-sid-6"
export KB_ARGV_SPY="$TMPROOT/argv6.txt"
export SLATE_JSON='{"text":"","head_seq":10}'
WORKTREE_CWD="/home/user/worktrees/slate-m1-linked"
printf '%s' "{\"session_id\":\"rc-sid-6\",\"cwd\":\"$WORKTREE_CWD\",\"prompt\":\"hi\"}" | "$RECALL" >/dev/null
spy="$(cat "$TMPROOT/argv6.txt" 2>/dev/null)"
unset KB_ARGV_SPY SLATE_JSON
case "$spy" in
  *"--cwd $WORKTREE_CWD"*) ok "worktree cwd forwarded verbatim to kb slate delta" ;;
  *) bad "worktree cwd forwarded verbatim to kb slate delta (got: $spy)" ;;
esac
case "$spy" in
  *"--since 10"*) ok "kb slate delta reads --since from the cursor file" ;;
  *) bad "kb slate delta reads --since from the cursor file (got: $spy)" ;;
esac
case "$spy" in
  *"--budget 1500"*"--json"*) ok "kb slate delta carries --budget 1500 --json" ;;
  *) bad "kb slate delta carries --budget 1500 --json (got: $spy)" ;;
esac

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
