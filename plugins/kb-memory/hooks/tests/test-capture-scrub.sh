#!/usr/bin/env bash
# test-capture-scrub.sh — v0.44 F7b + X4: every adapter that hand-writes its
# own envelope (the codex + opencode adapters, and the bash FALLBACK writers of
# the kimi, omp and grok adapters used when `kb sessions capture` fails) must
# run the translated transcript through the secrets-only scrubber
# (`kb sessions scrub`) before embedding it, and must FAIL CLOSED (write
# nothing) when the scrubber is unavailable.
#
# Uses the REAL `kb` binary (KB_BIN_DIR, set by the cargo harness, else PATH)
# for the positive cases and a fake failing `kb` for the fail-closed ones.
# Fixtures are synthetic; the canaries are the scrubber's own documented
# example token shapes.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-capture-scrub.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-capture-scrub-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

GH="ghp_0123456789abcdefghijklmnopqrstuvwxyzAB"
AWS="AKIAIOSFODNN7EXAMPLE"

REAL_PATH="$PATH"
[ -n "${KB_BIN_DIR:-}" ] && REAL_PATH="$KB_BIN_DIR:$PATH"

mkdir -p "$TMPROOT/failbin"
cat >"$TMPROOT/failbin/kb" <<'FAKE'
#!/usr/bin/env bash
exit 1
FAKE
chmod +x "$TMPROOT/failbin/kb"

# --- fixtures ---------------------------------------------------------------
ROLLOUT="$TMPROOT/rollout.jsonl"
cat >"$ROLLOUT" <<JSONL
{"timestamp":"2026-03-01T09:00:00.000Z","type":"session_meta","payload":{"id":"codex-sess-0001","timestamp":"2026-03-01T09:00:00.000Z","cwd":"/tmp/x","originator":"codex","cli_version":"0"}}
{"timestamp":"2026-03-01T09:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"use my key $AWS for s3"}]}}
{"timestamp":"2026-03-01T09:00:02.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"token $GH leaked"}}
JSONL

EXPORT="$TMPROOT/oc-export.json"
cat >"$EXPORT" <<JSON
{"info":{"id":"oc-sess-0001","directory":"/tmp/x","title":"t"},"messages":[{"info":{"role":"user","time":{"created":1772355600000}},"parts":[{"type":"text","text":"my key is $AWS ok"}]},{"info":{"role":"assistant","time":{"created":1772355601000}},"parts":[{"type":"tool","tool":"bash","callID":"c1","state":{"status":"completed","input":{"command":"echo hi"},"output":"token $GH here"}}]}]}
JSON

run_codex() { # $1 = PATH, $2 = sessions dir
  PATH="$1" KB_SESSIONS_DIR="$2" bash "$HOOKS_DIR/kb-capture-codex.sh" "$ROLLOUT" >/dev/null 2>&1
}
run_oc() {
  PATH="$1" KB_SESSIONS_DIR="$2" bash "$HOOKS_DIR/kb-capture-opencode.sh" "$EXPORT" >/dev/null 2>&1
}

check_scrubbed() { # name dir harness
  local name="$1" dir="$2" harness="$3" f
  f="$(ls "$dir"/session-*.html 2>/dev/null | head -1)"
  if [ -z "$f" ]; then bad "$name: a capture was written"; return; fi
  ok "$name: a capture was written"
  if grep -q "$GH\|$AWS" "$f"; then bad "$name: no raw secret reaches the artifact"; else ok "$name: no raw secret reaches the artifact"; fi
  if grep -q '\[redacted:' "$f"; then ok "$name: redaction markers present"; else bad "$name: redaction markers present"; fi
  if grep -q "name=\"kb-harness\" content=\"$harness\"" "$f"; then ok "$name: kb-harness meta preserved"; else bad "$name: kb-harness meta preserved"; fi
}

echo "== adapter secrets floor =="
D1="$TMPROOT/s-codex"; run_codex "$REAL_PATH" "$D1"; check_scrubbed codex "$D1" codex
D2="$TMPROOT/s-oc"; run_oc "$REAL_PATH" "$D2"; check_scrubbed opencode "$D2" opencode

echo "== fail closed when the scrubber is unavailable =="
D3="$TMPROOT/f-codex"; mkdir -p "$D3"; run_codex "$TMPROOT/failbin:$PATH" "$D3"
if ls "$D3"/session-*.html >/dev/null 2>&1; then bad "codex: nothing written on scrub failure"; else ok "codex: nothing written on scrub failure"; fi
D4="$TMPROOT/f-oc"; mkdir -p "$D4"; run_oc "$TMPROOT/failbin:$PATH" "$D4"
if ls "$D4"/session-*.html >/dev/null 2>&1; then bad "opencode: nothing written on scrub failure"; else ok "opencode: nothing written on scrub failure"; fi

echo "== bash-fallback writers of kimi, omp and grok (capture forced to fail) =="
# `sessions capture` fails (forcing the fallback writer); every other verb runs
# the REAL kb, so `sessions scrub` is the real scrubber.
mkdir -p "$TMPROOT/fbbin"
REAL_KB="$(PATH="$REAL_PATH" command -v kb || true)"
cat >"$TMPROOT/fbbin/kb" <<FAKE
#!/usr/bin/env bash
if [ "\${1:-}" = "sessions" ] && [ "\${2:-}" = "capture" ]; then exit 1; fi
exec "$REAL_KB" "\$@"
FAKE
chmod +x "$TMPROOT/fbbin/kb"

# kimi: wire.jsonl laid out as <home>/sessions/<wd>/<session>/agents/main/wire.jsonl
KSDIR="$TMPROOT/kimi-home/sessions/wd_x_deadbeef/session_aaaaaaaa-0000-0000-0000-000000000001/agents/main"
mkdir -p "$KSDIR"
sed "s/commit the widget fix please/commit the widget fix please $AWS/; s/wrote \/tmp\/widget.py/token $GH leaked/" \
  "$SCRIPT_DIR/fixtures/kimi-wire-commit.jsonl" >"$KSDIR/wire.jsonl"
# omp: fixed-width title slot + fixture body with a secret in the user prompt
OMPS="$TMPROOT/omp-session.jsonl"
{
  title='{"type":"title","v":1,"title":"t"}'
  printf '%s%*s\n' "$title" "$((256 - ${#title} - 1))" ''
  sed "s/commit the widget fix please/commit the widget fix please $AWS token $GH/" "$SCRIPT_DIR/fixtures/omp-session-commit.jsonl"
} >"$OMPS"
# grok: a session dir with a secret in a user message
GSD="$TMPROOT/grok-sd"
cp -r "$SCRIPT_DIR/fixtures/grok-session" "$GSD"
sed -i "s/Add a --dry-run flag to the fixture export command./Add a flag; my key is $AWS and $GH/" "$GSD/chat_history.jsonl"

run_fb() { # adapter PATH dir
  case "$1" in
    kimi) PATH="$2" KB_SESSIONS_DIR="$3" bash "$HOOKS_DIR/kb-capture-kimi.sh" "$KSDIR/wire.jsonl" >/dev/null 2>&1 ;;
    omp) PATH="$2" KB_SESSIONS_DIR="$3" bash "$HOOKS_DIR/kb-capture-omp.sh" "$OMPS" >/dev/null 2>&1 ;;
    grok) PATH="$2" KB_SESSIONS_DIR="$3" XDG_CACHE_HOME="$TMPROOT/cache-$1" bash "$HOOKS_DIR/kb-capture-grok.sh" --session-dir "$GSD" --cwd /tmp/x >/dev/null 2>&1 ;;
  esac
}
if [ -z "$REAL_KB" ]; then
  bad "fallback writers: a real kb binary is required (KB_BIN_DIR)"
else
  for h in kimi omp grok; do
    D="$TMPROOT/fb-$h"; mkdir -p "$D"
    run_fb "$h" "$TMPROOT/fbbin:$REAL_PATH" "$D"
    check_scrubbed "$h fallback" "$D" "$h"
    DF="$TMPROOT/fbf-$h"; mkdir -p "$DF"
    run_fb "$h" "$TMPROOT/failbin:$PATH" "$DF"
    if ls "$DF"/session-*.html >/dev/null 2>&1; then bad "$h fallback: nothing written on scrub failure"; else ok "$h fallback: nothing written on scrub failure"; fi
  done
fi

echo
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
