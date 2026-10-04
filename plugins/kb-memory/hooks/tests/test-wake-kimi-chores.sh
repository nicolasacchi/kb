#!/usr/bin/env bash
# test-wake-kimi-chores.sh — v0.45 N5: kb-wake-kimi.sh appends the
# `kb chores --line` output to its first-prompt payload (the kimi twin of
# test-wake-chores.sh).
#
# The CLI owns the once-a-day stamp and the "nothing due / daemon down =>
# print nothing" rule; this pins the hook half: the line rides the first
# prompt, only the FIRST line is taken, an old CLI / down daemon is silent,
# a hung CLI is bounded, and a second prompt of the same session emits
# nothing (the waked-kimi marker).
#
# Fake `kb` on PATH; `jq` is real.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-wake-kimi-chores.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WAKE="$(cd "$SCRIPT_DIR/.." && pwd)/kb-wake-kimi.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-wake-kimi-chores-test.XXXXXX")"
trap 'rm -rf "$TMPROOT"' EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

mkdir -p "$TMPROOT/bin"
export PATH="$TMPROOT/bin:$PATH"

cat >"$TMPROOT/bin/kb" <<'EOF2'
#!/usr/bin/env bash
case "$1" in
  chores)
    printf '%s skew=%s\n' "$*" "${KB_SKEW_SHOWN:-unset}" >>"$CHORES_ARGS_FILE"
    case "${CHORES_MODE:-line}" in
      line) echo "kb chores: 2 due (distill, memory-triage) — run \`kb chores\`"; exit 0 ;;
      multi) printf 'kb chores: 1 due (distill) — run `kb chores`\nINJECTED SECOND LINE\n'; exit 0 ;;
      silent) exit 0 ;;
      old) echo "error: unrecognized subcommand 'chores'" >&2; exit 2 ;;
      down) echo "daemon unreachable" >&2; exit 1 ;;
      slow) exec sleep 30 ;;
    esac
    ;;
esac
exit 0
EOF2
chmod +x "$TMPROOT/bin/kb"

run_wake() {
  export XDG_CACHE_HOME="$1"
  export CHORES_ARGS_FILE="$1/chores-args"
  mkdir -p "$1"
  printf '%s' '{"session_id":"wake-kimi-chores-sid","cwd":"/tmp/wake-kimi-chores-proj"}' \
    | "$WAKE"
}

echo "== kb-wake-kimi.sh chores line test matrix =="

export CHORES_MODE=line
out="$(run_wake "$TMPROOT/c1")"
case "$out" in *"kb chores: 2 due (distill, memory-triage)"*) ok "chores line rides first prompt" ;; *) bad "chores line rides first prompt (got: $out)" ;; esac
case "$(cat "$TMPROOT/c1/chores-args")" in "chores --line skew=0") ok "the hook asks for --line with KB_SKEW_SHOWN=0" ;; *) bad "the hook asks for --line with KB_SKEW_SHOWN=0 ($(cat "$TMPROOT/c1/chores-args"))" ;; esac
# The line is appended LAST.
last="$(printf '%s\n' "$out" | tail -n 1)"
case "$last" in "kb chores:"*) ok "the chores line is last" ;; *) bad "the chores line is last (got: $last)" ;; esac

# Same session, second prompt: the marker gates everything, chores included.
before="$(wc -l <"$TMPROOT/c1/chores-args")"
out2="$(run_wake "$TMPROOT/c1")"
after="$(wc -l <"$TMPROOT/c1/chores-args")"
if [ -z "$out2" ] && [ "$before" = "$after" ]; then ok "second prompt of same session emits nothing"; else bad "second prompt of same session emits nothing (got: $out2)"; fi

export CHORES_MODE=multi
out="$(run_wake "$TMPROOT/c2")"
case "$out" in *"INJECTED SECOND LINE"*) bad "only first line kept (multi)" ;; *"kb chores: 1 due"*) ok "only first line kept (multi)" ;; *) bad "only first line kept (multi): first line missing" ;; esac

export CHORES_MODE=old
out="$(run_wake "$TMPROOT/c3")"
case "$out" in *"kb chores:"*|*unrecognized*) bad "old CLI silent" ;; *) ok "old CLI silent" ;; esac

export CHORES_MODE=down
out="$(run_wake "$TMPROOT/c4")"
case "$out" in *"kb chores:"*|*unreachable*) bad "daemon down silent" ;; *) ok "daemon down silent" ;; esac

export CHORES_MODE=silent
out="$(run_wake "$TMPROOT/c5")"
case "$out" in *"kb chores:"*) bad "nothing due adds nothing" ;; *) ok "nothing due adds nothing" ;; esac

export CHORES_MODE=slow
start=$(date +%s)
out="$(run_wake "$TMPROOT/c6")"
el=$(( $(date +%s) - start ))
if [ "$el" -lt 5 ]; then ok "slow kb capped at 3s (total runtime ${el}s < 5s)"; else bad "slow kb took ${el}s"; fi

echo
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
