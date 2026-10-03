#!/usr/bin/env bash
# test-hook-identity.sh — v0.44 X4: the hooks publish KB_SESSION_ID and
# KB_HARNESS so a shell `kb` write (remember, slate, notes) made in or beside
# a session is attributed to it, not to the last-writer-wins marker.
#
#   * hook_export_identity exports both, and on Claude's SessionStart appends
#     the same two lines to $CLAUDE_ENV_FILE (the one channel that reaches the
#     agent's later Bash tool calls);
#   * kb-wake.sh (the real SessionStart hook) does so for a real payload, and
#     the `kb` it spawns sees the exported identity;
#   * post_distill_ask runs `kb slate ask` with the session's identity.
#
# Fake `kb` on PATH records its environment; `jq` is real.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-hook-identity.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-hook-identity-test.XXXXXX")"
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
printf '%s|sid=%s|harness=%s\n' "$1" "${KB_SESSION_ID:-}" "${KB_HARNESS:-}" >>"$KB_ENV_SPY"
case "$1" in
  recall) printf '%s' '{"hits":[]}' ;;
  slate) printf '%s' '{"text":"","head_seq":1}' ;;
esac
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
export KB_ENV_SPY="$TMPROOT/spy.log"
: >"$KB_ENV_SPY"
export XDG_CACHE_HOME="$TMPROOT/cache"
mkdir -p "$XDG_CACHE_HOME"

echo "== hook_export_identity =="
envfile="$TMPROOT/claude-env"
out="$(
  unset KB_SESSION_ID KB_HARNESS
  export CLAUDE_ENV_FILE="$envfile"
  . "$HOOKS_DIR/kb-hook-lib.sh"
  hook_export_identity "sid with 'quote" claude
  printf '%s|%s' "$KB_SESSION_ID" "$KB_HARNESS"
)"
[ "$out" = "sid with 'quote|claude" ] && ok "exports KB_SESSION_ID and KB_HARNESS" || bad "exports KB_SESSION_ID and KB_HARNESS ($out)"
# the env file must re-source to the same values (proves the quoting)
got="$(unset KB_SESSION_ID KB_HARNESS; . "$envfile"; printf '%s|%s' "$KB_SESSION_ID" "$KB_HARNESS")"
[ "$got" = "sid with 'quote|claude" ] && ok "CLAUDE_ENV_FILE re-sources to the same identity" || bad "CLAUDE_ENV_FILE re-sources ($got)"

out="$(
  unset KB_SESSION_ID KB_HARNESS CLAUDE_ENV_FILE
  . "$HOOKS_DIR/kb-hook-lib.sh"
  hook_export_identity "" claude
  hook_export_identity s1 ""
  printf '%s|%s' "${KB_SESSION_ID:-}" "${KB_HARNESS:-unset}"
)"
[ "$out" = "s1|unset" ] && ok "blank sid is a no-op; an unknown harness is not guessed" || bad "blank/unknown handling ($out)"

echo "== kb-wake.sh (real SessionStart hook) =="
envfile2="$TMPROOT/wake-env"
payload='{"session_id":"wake-ident-sid","cwd":"/tmp/wake-ident","source":"startup"}'
( unset KB_SESSION_ID KB_HARNESS; export CLAUDE_ENV_FILE="$envfile2"; printf '%s' "$payload" | "$HOOKS_DIR/kb-wake.sh" >/dev/null 2>&1 )
if grep -q 'wake-ident-sid' "$envfile2" 2>/dev/null && grep -q 'KB_HARNESS=claude' "$envfile2" 2>/dev/null; then
  ok "kb-wake.sh appends the identity to CLAUDE_ENV_FILE"
else
  bad "kb-wake.sh appends the identity to CLAUDE_ENV_FILE ($(cat "$envfile2" 2>/dev/null))"
fi
if grep -q '^recall|sid=wake-ident-sid|harness=claude$' "$KB_ENV_SPY"; then
  ok "the kb the hook itself spawns inherits the identity"
else
  bad "the kb the hook itself spawns inherits the identity ($(cat "$KB_ENV_SPY"))"
fi

echo "== post_distill_ask =="
: >"$KB_ENV_SPY"
( unset KB_SESSION_ID KB_HARNESS; . "$HOOKS_DIR/kb-hook-lib.sh"; post_distill_ask ask-sid kimi /tmp/x )
if grep -q '^slate|sid=ask-sid|harness=kimi$' "$KB_ENV_SPY"; then
  ok "the distill ask carries the session's identity in its environment"
else
  bad "the distill ask carries the session's identity ($(cat "$KB_ENV_SPY"))"
fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
