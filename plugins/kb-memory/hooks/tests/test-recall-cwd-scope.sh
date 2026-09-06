#!/usr/bin/env bash
# test-recall-cwd-scope.sh — project-aware recall: kb-recall.sh, kb-wake.sh,
# and kb-wake-kimi.sh must pass the payload's `--cwd` to their `kb recall`
# call and must NEVER pass `--scope all` (the CLI's new "auto" default scope
# — global corpora + the caller repo's own memory-<slug> corpus — is derived
# server-side from `--cwd`; hardcoding `--scope all` here would silently
# force the old fleet-wide view on every recall forever).
#
# Fake `kb` on PATH that spies on the `recall` subcommand's argv; `jq` is
# real.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-recall-cwd-scope.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RECALL="$HOOKS_DIR/kb-recall.sh"
WAKE="$HOOKS_DIR/kb-wake.sh"
WAKE_KIMI="$HOOKS_DIR/kb-wake-kimi.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-recall-cwd-scope-test.XXXXXX")"
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
if [ "$1" = "recall" ]; then
  if [ -n "${KB_ARGV_SPY:-}" ]; then
    printf '%s\n' "$@" >"$KB_ARGV_SPY"
  fi
  echo '{"hits":[]}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"

echo "== project-aware recall (--cwd, no --scope all) test matrix =="
echo "tmp root: $TMPROOT"
echo

# --- 1. kb-recall.sh forwards --cwd, drops --scope all --------------------
export XDG_CACHE_HOME="$TMPROOT/cache1"
export KB_ARGV_SPY="$TMPROOT/argv-recall.txt"
printf '%s' '{"session_id":"cs-1","cwd":"/home/user/project/kb","prompt":"hello"}' \
  | "$RECALL" >/dev/null
spy="$(cat "$KB_ARGV_SPY" 2>/dev/null)"
case "$spy" in
  *"--cwd"*"/home/user/project/kb"*) ok "kb-recall.sh forwards --cwd to kb recall" ;;
  *) bad "kb-recall.sh forwards --cwd to kb recall (got: $spy)" ;;
esac
case "$spy" in
  *"--scope"*"all"*) bad "kb-recall.sh must not pass --scope all (got: $spy)" ;;
  *) ok "kb-recall.sh never passes --scope all" ;;
esac

# --- 2. no cwd in the payload and no $PWD fallback match -> no --cwd arg --
# (can't unset $PWD portably; just confirm --cwd's value tracks the payload,
# not a hardcoded string, by varying it.)
export KB_ARGV_SPY="$TMPROOT/argv-recall2.txt"
printf '%s' '{"session_id":"cs-2","cwd":"/tmp/other-project","prompt":"hello"}' \
  | "$RECALL" >/dev/null
spy2="$(cat "$KB_ARGV_SPY" 2>/dev/null)"
case "$spy2" in
  *"--cwd"*"/tmp/other-project"*) ok "kb-recall.sh's --cwd tracks the payload cwd" ;;
  *) bad "kb-recall.sh's --cwd tracks the payload cwd (got: $spy2)" ;;
esac

# --- 3. kb-wake.sh forwards --cwd, drops --scope all -----------------------
export XDG_CACHE_HOME="$TMPROOT/cache3"
export KB_ARGV_SPY="$TMPROOT/argv-wake.txt"
printf '%s' '{"session_id":"cs-3","cwd":"/home/user/project/kb","hook_event_name":"SessionStart"}' \
  | "$WAKE" >/dev/null
spyw="$(cat "$KB_ARGV_SPY" 2>/dev/null)"
case "$spyw" in
  *"--cwd"*"/home/user/project/kb"*) ok "kb-wake.sh forwards --cwd to kb recall" ;;
  *) bad "kb-wake.sh forwards --cwd to kb recall (got: $spyw)" ;;
esac
case "$spyw" in
  *"--scope"*"all"*) bad "kb-wake.sh must not pass --scope all (got: $spyw)" ;;
  *) ok "kb-wake.sh never passes --scope all" ;;
esac

# --- 4. kb-wake-kimi.sh forwards --cwd, drops --scope all ------------------
export XDG_CACHE_HOME="$TMPROOT/cache4"
export KB_ARGV_SPY="$TMPROOT/argv-wake-kimi.txt"
printf '%s' '{"session_id":"cs-4","cwd":"/home/user/project/kb","hook_event_name":"UserPromptSubmit"}' \
  | "$WAKE_KIMI" >/dev/null
spyk="$(cat "$KB_ARGV_SPY" 2>/dev/null)"
case "$spyk" in
  *"--cwd"*"/home/user/project/kb"*) ok "kb-wake-kimi.sh forwards --cwd to kb recall" ;;
  *) bad "kb-wake-kimi.sh forwards --cwd to kb recall (got: $spyk)" ;;
esac
case "$spyk" in
  *"--scope"*"all"*) bad "kb-wake-kimi.sh must not pass --scope all (got: $spyk)" ;;
  *) ok "kb-wake-kimi.sh never passes --scope all" ;;
esac

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
