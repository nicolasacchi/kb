#!/usr/bin/env bash
# test-capture-scrub.sh — v0.44 F7b + X4, reworked in v0.45 N4: none of the
# codex/opencode/kimi/omp/grok adapters writes session HTML itself any more.
# Each hands its translated transcript to `kb sessions capture` (the Rust
# engine, which applies the secrets-only scrub), and when that cannot run it
# parks the translation in the private spool instead. So: with the REAL kb, no
# raw secret reaches the artifact and the harness survives (adapter-meta, the
# enrich ladder's rung 1); with a failing/absent kb, NOTHING is written to the
# corpus.
#
# Uses the REAL `kb` binary (KB_BIN_DIR, set by the cargo harness, else PATH)
# for the positive cases and a fake failing `kb` for the failure ones.
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

check_scrubbed() { # name dir harness
  local name="$1" dir="$2" harness="$3" f
  f="$(ls "$dir"/session-*.html 2>/dev/null | head -1)"
  if [ -z "$f" ]; then bad "$name: a capture was written"; return; fi
  ok "$name: a capture was written"
  if grep -q "$GH\|$AWS" "$f"; then bad "$name: no raw secret reaches the artifact"; else ok "$name: no raw secret reaches the artifact"; fi
  if grep -q '\[redacted:' "$f"; then ok "$name: redaction markers present"; else bad "$name: redaction markers present"; fi
  # The harness rides the adapter-meta record inside the <pre> (ladder rung 1).
  if grep -q "\"harness\":\"$harness\"" "$f"; then ok "$name: harness $harness survives in adapter-meta"; else bad "$name: harness $harness survives in adapter-meta"; fi
}

# Fixtures for the three adapters that need an on-disk layout.
KSDIR="$TMPROOT/kimi-home/sessions/wd_x_deadbeef/session_aaaaaaaa-0000-0000-0000-000000000001/agents/main"
mkdir -p "$KSDIR"
sed "s/commit the widget fix please/commit the widget fix please $AWS/; s/wrote \/tmp\/widget.py/token $GH leaked/" \
  "$SCRIPT_DIR/fixtures/kimi-wire-commit.jsonl" >"$KSDIR/wire.jsonl"
OMPS="$TMPROOT/omp-session.jsonl"
{
  title='{"type":"title","v":1,"title":"t"}'
  printf '%s%*s\n' "$title" "$((256 - ${#title} - 1))" ''
  sed "s/commit the widget fix please/commit the widget fix please $AWS token $GH/" "$SCRIPT_DIR/fixtures/omp-session-commit.jsonl"
} >"$OMPS"
GSD="$TMPROOT/grok-sd"
cp -r "$SCRIPT_DIR/fixtures/grok-session" "$GSD"
sed -i "s/Add a --dry-run flag to the fixture export command./Add a flag; my key is $AWS and $GH/" "$GSD/chat_history.jsonl"

run_adapter() { # adapter PATH dir [spool]
  local spool="${4:-$TMPROOT/spool-$1}"
  case "$1" in
    codex) PATH="$2" KB_SESSIONS_DIR="$3" KB_CAPTURE_SPOOL="$spool" bash "$HOOKS_DIR/kb-capture-codex.sh" "$ROLLOUT" >/dev/null 2>&1 ;;
    opencode) PATH="$2" KB_SESSIONS_DIR="$3" KB_CAPTURE_SPOOL="$spool" bash "$HOOKS_DIR/kb-capture-opencode.sh" "$EXPORT" >/dev/null 2>&1 ;;
    kimi) PATH="$2" KB_SESSIONS_DIR="$3" KB_CAPTURE_SPOOL="$spool" bash "$HOOKS_DIR/kb-capture-kimi.sh" "$KSDIR/wire.jsonl" >/dev/null 2>&1 ;;
    omp) PATH="$2" KB_SESSIONS_DIR="$3" KB_CAPTURE_SPOOL="$spool" bash "$HOOKS_DIR/kb-capture-omp.sh" "$OMPS" >/dev/null 2>&1 ;;
    grok) PATH="$2" KB_SESSIONS_DIR="$3" KB_CAPTURE_SPOOL="$spool" XDG_CACHE_HOME="$TMPROOT/cache-$1" bash "$HOOKS_DIR/kb-capture-grok.sh" --session-dir "$GSD" --cwd /tmp/x >/dev/null 2>&1 ;;
  esac
}

REAL_KB="$(PATH="$REAL_PATH" command -v kb || true)"
if [ -z "$REAL_KB" ]; then
  bad "adapters: a real kb binary is required (KB_BIN_DIR)"
else
  for h in codex opencode kimi omp grok; do
    echo "== $h: real kb scrubs, harness survives =="
    D="$TMPROOT/s-$h"; run_adapter "$h" "$REAL_PATH" "$D"; check_scrubbed "$h" "$D" "$h"
    echo "== $h: a failing kb writes nothing to the corpus =="
    DF="$TMPROOT/f-$h"; mkdir -p "$DF"; run_adapter "$h" "$TMPROOT/failbin:$PATH" "$DF"
    if ls "$DF"/session-*.html >/dev/null 2>&1; then bad "$h: nothing written to the corpus on capture failure"; else ok "$h: nothing written to the corpus on capture failure"; fi
    if grep -rq "$GH\|$AWS" "$DF" 2>/dev/null; then bad "$h: no raw secret in the corpus dir"; else ok "$h: no raw secret in the corpus dir"; fi
  done
fi

echo "== kb resolution when the hook PATH lacks it =="
# A minimal PATH (jq + coreutils only) with `kb` installed under $HOME/.local/bin:
# the adapter must find it there instead of silently skipping the capture. With
# no kb anywhere nothing reaches the corpus, the translation is spooled, and
# the stderr line says so.
if [ -n "$REAL_KB" ] && command -v jq >/dev/null 2>&1; then
  MINPATH="/usr/bin:/bin:$(dirname "$(command -v jq)")"
  skip_min=0
  PATH="$MINPATH" command -v kb >/dev/null 2>&1 && skip_min=1
  HOME1="$TMPROOT/home-with-kb"; mkdir -p "$HOME1/.local/bin"; ln -sf "$REAL_KB" "$HOME1/.local/bin/kb"
  DR="$TMPROOT/r-codex"; mkdir -p "$DR"
  if [ "$skip_min" = 1 ]; then
    ok "kb-on-minimal-PATH check skipped (kb already lives on /usr/bin)"
  else
    env -i PATH="$MINPATH" HOME="$HOME1" KB_SESSIONS_DIR="$DR" bash "$HOOKS_DIR/kb-capture-codex.sh" "$ROLLOUT" >/dev/null 2>&1
    check_scrubbed "codex (kb found via ~/.local/bin)" "$DR" codex
    DR2="$TMPROOT/r-oc"; mkdir -p "$DR2"
    env -i PATH="$MINPATH" HOME="$HOME1" KB_SESSIONS_DIR="$DR2" bash "$HOOKS_DIR/kb-capture-opencode.sh" "$EXPORT" >/dev/null 2>&1
    check_scrubbed "opencode (kb found via ~/.local/bin)" "$DR2" opencode
    DR3="$TMPROOT/r-none"; mkdir -p "$DR3" "$TMPROOT/home-empty"
    err="$(env -i PATH="$MINPATH" HOME="$TMPROOT/home-empty" KB_SESSIONS_DIR="$DR3" bash "$HOOKS_DIR/kb-capture-codex.sh" "$ROLLOUT" 2>&1 >/dev/null)"
    if ls "$DR3"/session-*.html >/dev/null 2>&1; then bad "codex: no kb anywhere writes nothing to the corpus"; else ok "codex: no kb anywhere writes nothing to the corpus"; fi
    case "$err" in *"spooled session"*) ok "codex: the spooling is named on stderr" ;; *) bad "codex: the spooling is named on stderr ($err)" ;; esac
  fi
fi

echo
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
