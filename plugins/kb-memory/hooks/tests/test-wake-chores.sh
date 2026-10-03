#!/usr/bin/env bash
# test-wake-chores.sh — v0.44 F10: kb-wake.sh appends the `kb chores --line`
# output to the SessionStart context.
#
# The CLI owns the once-a-day stamp and the "nothing due / daemon down =>
# print nothing" rule (pinned by the kb-cli chores tests); this pins the hook
# half: the line rides the context when the CLI prints one, nothing is added
# when it prints nothing or fails (an old CLI), only the FIRST line is taken,
# and a hung CLI is bounded.
#
# Fake `kb` on PATH; `jq` is real.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-wake-chores.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WAKE="$(cd "$SCRIPT_DIR/.." && pwd)/kb-wake.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-wake-chores-test.XXXXXX")"
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
    printf '%s\n' "$*" >>"$CHORES_ARGS_FILE"
    case "${CHORES_MODE:-line}" in
      line) echo "kb chores: 2 due (distill, memory-triage) — run \`kb chores\`"; exit 0 ;;
      multi) printf 'kb chores: 1 due (distill) — run `kb chores`\nINJECTED SECOND LINE\n'; exit 0 ;;
      silent) exit 0 ;;
      old) echo "error: unrecognized subcommand 'chores'" >&2; exit 2 ;;
      hang) exec sleep 30 ;;
    esac
    ;;
  version) echo 1; exit 0 ;;
esac
exit 0
EOF2
chmod +x "$TMPROOT/bin/kb"

run_wake() {
  export XDG_CACHE_HOME="$1"
  export CHORES_ARGS_FILE="$1/chores-args"
  mkdir -p "$1"
  printf '%s' '{"session_id":"wake-chores-sid","cwd":"/tmp/wake-chores-proj"}' \
    | "$WAKE" | jq -r '.hookSpecificOutput.additionalContext'
}

echo "== kb-wake.sh chores line test matrix =="

export CHORES_MODE=line
out="$(run_wake "$TMPROOT/c1")"
case "$out" in *"kb chores: 2 due (distill, memory-triage)"*) ok "the chores line rides the SessionStart context" ;; *) bad "the chores line rides the SessionStart context" ;; esac
case "$(cat "$TMPROOT/c1/chores-args")" in "chores --line"*) ok "the hook asks for --line" ;; *) bad "the hook asks for --line" ;; esac

export CHORES_MODE=silent
out="$(run_wake "$TMPROOT/c2")"
case "$out" in *"kb chores:"*) bad "an empty CLI answer must add nothing" ;; *) ok "nothing due / daemon down adds nothing" ;; esac

export CHORES_MODE=old
out="$(run_wake "$TMPROOT/c3")"
case "$out" in *"kb chores:"*|*unrecognized*) bad "an old CLI's failure must not surface" ;; *) ok "a CLI without the verb is silent" ;; esac

export CHORES_MODE=multi
out="$(run_wake "$TMPROOT/c4")"
case "$out" in *"INJECTED SECOND LINE"*) bad "only the first line may be taken" ;; *) ok "only the first line is taken" ;; esac

export CHORES_MODE=hang
start=$(date +%s)
out="$(run_wake "$TMPROOT/c5")"
el=$(( $(date +%s) - start ))
if [ "$el" -le 8 ]; then ok "a hung chores call is bounded (${el}s)"; else bad "a hung chores call took ${el}s"; fi

echo
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
