#!/usr/bin/env bash
# test-capture-scrub.sh — v0.44 F7b: the codex + opencode capture adapters
# hand-write their own envelope, so they must run the translated transcript
# through the secrets-only scrubber (`kb sessions scrub`) before embedding it,
# and must FAIL CLOSED (write nothing) when the scrubber is unavailable.
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

echo
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
