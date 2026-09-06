#!/usr/bin/env bash
# test-distill-nudge.sh — self-contained test matrix for
# kb-distill-nudge.sh (MI-W0.3): success-aware suppression (a `kb remember`
# that FAILED must not suppress the nudge) + Task-tool subagent sidecar
# scanning (a commit made entirely inside a subagent must still trigger
# it). Runs the REAL script against mktemp fixture transcripts, with a
# fake `kb` on PATH (only `command -v kb` needs to succeed — this script
# never actually calls it).
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-distill-nudge.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
NUDGE="$HOOKS_DIR/kb-distill-nudge.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-distill-nudge-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

mkdir -p "$TMPROOT/bin"
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
export PATH="$TMPROOT/bin:$PATH"
export KB_SESSIONS_DIR="$TMPROOT/sessions"
mkdir -p "$KB_SESSIONS_DIR"

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

# run_nudge <transcript-path> <session-id> <cache-dir>
run_nudge() {
  local tpath="$1" sid="$2" cache="$3"
  export XDG_CACHE_HOME="$cache"
  mkdir -p "$cache"
  printf '%s' "{\"transcript_path\":\"$tpath\",\"session_id\":\"$sid\"}" | "$NUDGE"
}

# fires <desc> <transcript-path> <session-id>
fires() {
  local desc="$1" tpath="$2" sid="$3" out
  out="$(run_nudge "$tpath" "$sid" "$TMPROOT/cache-$sid")"
  if printf '%s' "$out" | grep -q '"systemMessage"'; then
    ok "$desc"
  else
    bad "$desc (expected a systemMessage; got: $out)"
  fi
}

# suppressed <desc> <transcript-path> <session-id>
suppressed() {
  local desc="$1" tpath="$2" sid="$3" out
  out="$(run_nudge "$tpath" "$sid" "$TMPROOT/cache-$sid")"
  if [ -z "$out" ]; then
    ok "$desc"
  else
    bad "$desc (expected silence; got: $out)"
  fi
}

# --- fixtures -------------------------------------------------------------

# t1: git commit, no `kb remember` at all.
mkdir -p "$TMPROOT/t1"
cat >"$TMPROOT/t1/transcript.jsonl" <<'JSONL'
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"do a commit"}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"tu1","name":"Bash","input":{"command":"git add -A && git commit -m 'wip'"}}]}}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":[{"type":"text","text":"[main abc1234] wip"}]}]}}
JSONL

# t2: t1 + a SUCCESSFUL kb remember (the success marker present).
mkdir -p "$TMPROOT/t2"
cp "$TMPROOT/t1/transcript.jsonl" "$TMPROOT/t2/transcript.jsonl"
cat >>"$TMPROOT/t2/transcript.jsonl" <<'JSONL'
{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"tu2","name":"Bash","input":{"command":"kb remember 'fact'"}}]}}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu2","content":[{"type":"text","text":"remembered abc123def456  (/kb/memory/x.html)"}]}]}}
JSONL

# t3: parent transcript is clean; the commit happened inside a Task-tool
# subagent sidecar.
mkdir -p "$TMPROOT/t3/sess-3/subagents"
cat >"$TMPROOT/t3/transcript.jsonl" <<'JSONL'
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"delegate to a subagent"}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"spawning a subagent"}]}}
JSONL
cat >"$TMPROOT/t3/sess-3/subagents/agent1.jsonl" <<'JSONL'
{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"tu9","name":"Bash","input":{"command":"git commit -m 'from subagent'"}}]}}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu9","content":[{"type":"text","text":"[main def5678] from subagent"}]}]}}
JSONL

# t4: regression fixture for the old bug — the `kb remember` COMMAND is
# present but it FAILED (no success marker in the output). The nudge
# must still fire.
mkdir -p "$TMPROOT/t4"
cp "$TMPROOT/t1/transcript.jsonl" "$TMPROOT/t4/transcript.jsonl"
cat >>"$TMPROOT/t4/transcript.jsonl" <<'JSONL'
{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"tu3","name":"Bash","input":{"command":"kb remember 'fact'"}}]}}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu3","content":[{"type":"text","text":"Error: daemon unreachable (connection refused)"}]}]}}
JSONL

echo "== kb-distill-nudge.sh test matrix (MI-W0.3) =="
echo "tmp root: $TMPROOT"
echo

fires      "commit, no success marker -> fires"                                "$TMPROOT/t1/transcript.jsonl" "sess-1"
suppressed "commit + success marker -> suppressed"                             "$TMPROOT/t2/transcript.jsonl" "sess-2"
fires      "sidecar-only commit -> fires"                                      "$TMPROOT/t3/transcript.jsonl" "sess-3"
fires      "regression: kb remember command present but FAILED -> still fires" "$TMPROOT/t4/transcript.jsonl" "sess-4"
fires      "no sidecar dir present -> unchanged single-file behavior"          "$TMPROOT/t1/transcript.jsonl" "sess-nosidecar"

# Once-per-session marker: a second run against the SAME cache dir must
# stay silent even though the transcript still matches.
out2="$(run_nudge "$TMPROOT/t1/transcript.jsonl" "sess-1" "$TMPROOT/cache-sess-1")"
if [ -z "$out2" ]; then
  ok "marker caps the nudge at once per session"
else
  bad "marker caps the nudge at once per session (got: $out2)"
fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
