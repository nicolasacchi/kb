#!/usr/bin/env bash
# test-capture-kimi.sh — self-contained test matrix for kb-capture-kimi.sh:
# the Kimi Code wire.jsonl → Claude-shaped session-capture adapter. Runs
# the REAL script in CLI (backfill) mode against a checked-in synthesized
# fixture (tests/fixtures/kimi-wire-commit.jsonl) laid out in the real
# on-disk shape (<session_dir>/agents/main/wire.jsonl), with a fake `kb`
# on PATH whose `sessions capture` FAILS — forcing the bash-fallback write
# path so the test is hermetic (same trick as test-grok-distill-pending.sh).
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-capture-kimi.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CAPTURE="$HOOKS_DIR/kb-capture-kimi.sh"
FIXTURE="$SCRIPT_DIR/fixtures/kimi-wire-commit.jsonl"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-capture-kimi-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

mkdir -p "$TMPROOT/bin"
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
# `kb sessions capture` fails -> forces kb-capture-kimi.sh's bash-fallback
# write path, keeping this test hermetic (no real kb binary / daemon).
if [ "$1" = "sessions" ] && [ "$2" = "capture" ]; then exit 1; fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
export PATH="$TMPROOT/bin:$PATH"

export KB_SESSIONS_DIR="$TMPROOT/sessions"
mkdir -p "$KB_SESSIONS_DIR"

# Lay the fixture out in the real on-disk shape — the CLI mode derives the
# session id from the session dir name three levels above wire.jsonl.
SID="session_11112222-3333-4444-5555-666677778888"
SAFE_SID="session-11112222-3333-4444-5555-666677778888"
SDIR="$TMPROOT/kimi-home/sessions/wd_fixture_deadbeefcafe/$SID/agents/main"
mkdir -p "$SDIR"
cp "$FIXTURE" "$SDIR/wire.jsonl"

echo "== kb-capture-kimi.sh test matrix =="
echo "tmp root: $TMPROOT"
echo

"$CAPTURE" "$SDIR/wire.jsonl" >/dev/null 2>&1

n="$(find "$KB_SESSIONS_DIR" -name "session-*-$SAFE_SID.html" | wc -l | tr -d ' ')"
if [ "$n" = "1" ]; then
  ok "CLI capture writes exactly one session-*-<sid>.html"
else
  bad "CLI capture writes exactly one session-*-<sid>.html (found $n)"
fi

out="$(find "$KB_SESSIONS_DIR" -name "session-*-$SAFE_SID.html" | head -1)"

if grep -q 'kb-harness" content="kimi"' "$out"; then
  ok 'envelope carries kb-harness "kimi"'
else
  bad 'envelope carries kb-harness "kimi"'
fi

first="$(awk '/<pre>/{sub(/.*<pre>/,""); print; exit}' "$out")"
if printf '%s' "$first" | jq -e '.type == "adapter-meta" and .harness == "kimi" and .adapter == "kb-capture-kimi/1"' >/dev/null 2>&1; then
  ok 'embedded JSONL first line is adapter-meta with "harness":"kimi"'
else
  bad "embedded JSONL first line is adapter-meta with \"harness\":\"kimi\" (got: $first)"
fi

if grep -q '"type":"tool_use","id":"tool_fxc1","name":"Bash","input":{"command":"git add widget.py' "$out"; then
  ok "git commit Bash tool.call survived translation as tool_use Bash"
else
  bad "git commit Bash tool.call survived translation as tool_use Bash"
fi

# Write .args.path must land as file_path (the key parse_session_activity
# reads for the edited-set).
if grep -q '"name":"Write","input":{"path":"/tmp/widget.py","content":"print(' "$out"; then
  bad "Write tool_use input maps .args.path -> file_path (raw .path survived)"
elif grep -q '"name":"Write","input":{[^}]*"file_path":"/tmp/widget.py"' "$out"; then
  ok "Write tool_use input maps .args.path -> file_path"
else
  bad "Write tool_use input maps .args.path -> file_path"
fi

# Re-capture overwrites in place — still exactly one file.
"$CAPTURE" "$SDIR/wire.jsonl" >/dev/null 2>&1
n2="$(find "$KB_SESSIONS_DIR" -name "session-*-$SAFE_SID.html" | wc -l | tr -d ' ')"
if [ "$n2" = "1" ]; then
  ok "re-capture overwrites in place (still one file)"
else
  bad "re-capture overwrites in place (found $n2 files)"
fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
