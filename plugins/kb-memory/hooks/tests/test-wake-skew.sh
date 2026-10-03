#!/usr/bin/env bash
# test-wake-skew.sh — v0.44 F8: kb-wake.sh names CLI/hook version skew.
#
# The hook declares KB_HOOK_CONTRACT; `kb version --contract` is the CLI's.
# A CLI that prints less, or fails the call (an old binary that predates the
# verb), gets ONE line per day. A current CLI, an unknown answer (rc 0 and
# empty output), a probe timeout and KB_SKEW_NOTICE=0 all stay silent.
#
# Fake `kb` on PATH; `jq` is real.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-wake-skew.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WAKE="$HOOKS_DIR/kb-wake.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-wake-skew-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

mkdir -p "$TMPROOT/bin"
export PATH="$TMPROOT/bin:$PATH"

cat >"$TMPROOT/bin/kb" <<'EOF2'
#!/usr/bin/env bash
case "$1" in
  version)
    case "${CONTRACT_MODE:-current}" in
      current) echo 1; exit 0 ;;
      lower) echo 0; exit 0 ;;
      old) echo "error: unrecognized subcommand 'version'" >&2; exit 2 ;;
      empty) exit 0 ;;
      hang) exec sleep 30 ;;
    esac
    ;;
esac
exit 0
EOF2
chmod +x "$TMPROOT/bin/kb"

run_wake() {
  local cache="$1"
  export XDG_CACHE_HOME="$cache"
  mkdir -p "$cache"
  printf '%s' '{"session_id":"wake-skew-sid","cwd":"/tmp/wake-skew-proj"}' | "$WAKE" | jq -r '.hookSpecificOutput.additionalContext'
}

has_notice() { case "$1" in *"kb CLI is older than these hooks"*) return 0 ;; *) return 1 ;; esac; }

echo "== kb-wake.sh CLI-skew notice test matrix =="

export CONTRACT_MODE=current
if has_notice "$(run_wake "$TMPROOT/c1")"; then bad "a current CLI must not be nagged"; else ok "a current CLI is silent"; fi

export CONTRACT_MODE=lower
out="$(run_wake "$TMPROOT/c2")"
if has_notice "$out"; then ok "a lower contract names the skew"; else bad "a lower contract names the skew"; fi
case "$out" in *"KB_SKEW_NOTICE=0"*) ok "the notice says how to silence it" ;; *) bad "the notice says how to silence it" ;; esac
out2="$(run_wake "$TMPROOT/c2")"
if has_notice "$out2"; then bad "the notice repeats within the same day"; else ok "the notice is rate-limited to once a day"; fi
printf '1999-01-01\n' >"$TMPROOT/c2/kb/skew-notice"
if has_notice "$(run_wake "$TMPROOT/c2")"; then ok "the notice returns the next day"; else bad "the notice returns the next day"; fi

export CONTRACT_MODE=old
if has_notice "$(run_wake "$TMPROOT/c3")"; then ok "a binary that fails the call is older than contract 1"; else bad "a binary that fails the call is older than contract 1"; fi

export CONTRACT_MODE=empty
if has_notice "$(run_wake "$TMPROOT/c4")"; then bad "an unknown answer must not nag"; else ok "rc 0 with no output is unknown, not skew"; fi

export CONTRACT_MODE=hang
start=$(date +%s)
out="$(run_wake "$TMPROOT/c5")"
el=$(( $(date +%s) - start ))
if has_notice "$out"; then bad "a probe timeout must not be reported as skew"; else ok "a probe timeout is not skew"; fi
if [ "$el" -le 6 ]; then ok "the probe is bounded (${el}s)"; else bad "the probe took ${el}s"; fi

export CONTRACT_MODE=lower
if has_notice "$(KB_SKEW_NOTICE=0 run_wake "$TMPROOT/c6")"; then bad "KB_SKEW_NOTICE=0 must silence the notice"; else ok "KB_SKEW_NOTICE=0 silences the notice"; fi

echo
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
