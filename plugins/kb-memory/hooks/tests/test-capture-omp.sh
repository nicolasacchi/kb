#!/usr/bin/env bash
# test-capture-omp.sh — self-contained test matrix for kb-capture-omp.sh:
# the Oh My Pi session JSONL → Claude-shaped session-capture adapter. Runs
# the REAL script in CLI (backfill) mode against checked-in synthesized
# fixtures (tests/fixtures/omp-session-*.jsonl) shaped like live omp v3
# session files, with a fake `kb` on PATH whose `sessions capture` FAILS —
# forcing the bash-fallback write path so the test is hermetic (same trick
# as test-capture-kimi.sh). A title-slot line is prepended at runtime (the
# fixed-width 256-byte first line omp writes) to prove it is dropped.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-capture-omp.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CAPTURE="$HOOKS_DIR/kb-capture-omp.sh"
FIXTURE="$SCRIPT_DIR/fixtures/omp-session-commit.jsonl"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-capture-omp-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

mkdir -p "$TMPROOT/bin"
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = "sessions" ] && [ "${2:-}" = "capture" ]; then exit 1; fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
# OK4 — saved before the fake-kb override below, so the subagent-sidecar
# block near the end can shell out to the REAL installed kb binary (the
# Rust sidecar digest walk is not exercised by the bash-fallback path the
# rest of this file deliberately forces).
REAL_PATH="$PATH"
export PATH="$TMPROOT/bin:$PATH"

export KB_SESSIONS_DIR="$TMPROOT/sessions"
mkdir -p "$KB_SESSIONS_DIR"

SID="fx01a034-0000-0000-0000-000000000001"
SAFE_SID="fx01a034-0000-0000-0000-000000000001"

# Real on-disk shape: fixed-width title slot first, then the fixture body.
SESSION="$TMPROOT/session.jsonl"
{
  title='{"type":"title","v":1,"title":"t"}'
  pad_len=$((256 - ${#title} - 1))
  printf '%s%*s\n' "$title" "$pad_len" ''
  cat "$FIXTURE"
} >"$SESSION"

echo "== kb-capture-omp.sh test matrix =="
echo "tmp root: $TMPROOT"
echo

"$CAPTURE" "$SESSION" >/dev/null 2>&1

out="$(find "$KB_SESSIONS_DIR" -name "session-*-$SAFE_SID.html" | head -1)"
if [ -n "$out" ]; then
  ok "CLI mode wrote exactly the fallback HTML for the header session id"
else
  bad "CLI mode wrote no capture file"
  echo "passed=$PASS failed=$FAIL"; exit 1
fi

if grep -q 'kb-harness" content="omp"' "$out"; then
  ok "envelope carries kb-harness: omp"
else
  bad "envelope missing kb-harness: omp"
fi

first="$(awk '/<pre>/{sub(/.*<pre>/,""); print; exit}' "$out")"
if printf '%s' "$first" | jq -e '.type == "adapter-meta" and .harness == "omp" and .adapter == "kb-capture-omp/1"' >/dev/null 2>&1; then
  ok "first translated line is the omp adapter-meta"
else
  bad "first line is not the omp adapter-meta: $first"
fi

if printf '%s' "$first" | jq -e '.session_file != "" and .cwd == "/tmp/kb-omp-fixture"' >/dev/null 2>&1; then
  ok "adapter-meta records source path and header cwd"
else
  bad "adapter-meta missing session_file/cwd"
fi

body="$(sed -n '/<pre>/,/<\/pre>/p' "$out" | sed '1d;$s|</pre>$||')"

# Leaf chain: the abandoned branch (b1) must never appear; main chain does.
if printf '%s\n' "$body" | grep -q 'ABANDONED BRANCH LINE'; then
  bad "abandoned branch entry leaked into the capture (leaf-chain filter broken)"
else
  ok "abandoned branch entry excluded by leaf-chain resolution"
fi
if printf '%s\n' "$body" | grep -q '"type":"title"'; then
  bad "fixed-width title slot leaked into the capture"
else
  ok "title-slot line dropped"
fi

# Bash tool call canonicalization + command preserved for commit detection.
if printf '%s\n' "$body" | grep -q '"type":"tool_use","id":"fc_1","name":"Bash","input":{"command":"git add widget.py'; then
  ok "bash toolCall → tool_use name=Bash with command intact"
else
  bad "bash tool_use translation wrong"
fi

# Write/Edit path aliasing — the key parse_session_activity reads.
# (arguments keys keep their original order; only presence is asserted.)
if printf '%s\n' "$body" | grep -q '"name":"Edit"' && \
   printf '%s\n' "$body" | grep -q '"file_path":"/tmp/widget.py"'; then
  ok "edit arguments.path aliased as file_path"
else
  bad "file_path aliasing missing"
fi

# toolResult → user tool_result with is_error passthrough.
n_results="$(printf '%s\n' "$body" | grep -c '"type":"tool_result"' || true)"
if [ "$n_results" = "2" ]; then
  ok "both toolResults translated ($n_results)"
else
  bad "expected 2 tool_result lines, got $n_results"
fi

# Per-assistant usage lines (input+cacheRead+cacheWrite summed).
if printf '%s\n' "$body" | grep -q '"usage":{"input_tokens":1000,"output_tokens":20}'; then
  ok "assistant usage sums input+cacheRead+cacheWrite"
else
  bad "usage translation wrong"
fi

# Trailing edited-set snapshot from Write/Edit paths.
last="$(printf '%s\n' "$body" | tail -1)"
if printf '%s' "$last" | jq -e '.type == "file-history-snapshot" and .snapshot.trackedFileBackups["/tmp/widget.py"] == {}' >/dev/null 2>&1; then
  ok "trailing file-history-snapshot lists the edited file"
else
  bad "file-history-snapshot wrong: $last"
fi

# Re-capture overwrites in place — still exactly one file.
"$CAPTURE" "$SESSION" >/dev/null 2>&1
n="$(find "$KB_SESSIONS_DIR" -name "session-*-$SAFE_SID.html" | wc -l | tr -d ' ')"
if [ "$n" = "1" ]; then
  ok "re-capture overwrites the same per-session file"
else
  bad "re-capture created a duplicate ($n files)"
fi

# Hook mode: stdin {session_file, session_id, cwd}.
rm -f "$KB_SESSIONS_DIR"/session-*-$SAFE_SID.html
printf '{"session_file":"%s","session_id":"hooksid1","cwd":"/tmp/kb-omp-fixture"}\n' "$SESSION" \
  | "$CAPTURE" >/dev/null 2>&1
out2="$(find "$KB_SESSIONS_DIR" -name "session-*-hooksid1.html" | head -1)"
if [ -n "$out2" ] && grep -q '"sessionId":"hooksid1"' "$out2"; then
  ok "hook mode reads stdin payload and honors explicit session_id"
else
  bad "hook mode produced no capture for stdin payload"
fi

# Malformed tail line must not kill the capture (lenient pre-clean).
rm -f "$KB_SESSIONS_DIR"/session-*.html
cp "$FIXTURE" "$TMPROOT/torn.jsonl"
printf '{"type":"message","id":"torn","parentId":"a5","timestamp":"2026-08-24T10:0' >>"$TMPROOT/torn.jsonl"
"$CAPTURE" "$TMPROOT/torn.jsonl" >/dev/null 2>&1
n="$(find "$KB_SESSIONS_DIR" -name "session-*-$SAFE_SID.html" | wc -l | tr -d ' ')"
if [ "$n" = "1" ]; then
  ok "torn trailing line tolerated (lenient parse)"
else
  bad "torn trailing line killed the capture"
fi

echo
echo "== OK4: intent join / compaction / recall ledger / session-exit =="

OK4_FIXTURE="$SCRIPT_DIR/fixtures/omp-session-ok4.jsonl"
rm -f "$KB_SESSIONS_DIR"/session-*.html
OK4_SESSION="$TMPROOT/ok4.jsonl"
{
  title='{"type":"title","v":1,"title":"OK4 test title"}'
  pad_len=$((256 - ${#title} - 1))
  printf '%s%*s\n' "$title" "$pad_len" ''
  cat "$OK4_FIXTURE"
} >"$OK4_SESSION"
"$CAPTURE" "$OK4_SESSION" >/dev/null 2>&1
ok4_out="$(find "$KB_SESSIONS_DIR" -name 'session-*-ok4sess1.html' | head -1)"
ok4_body="$(sed -n '/<pre>/,/<\/pre>/p' "$ok4_out" 2>/dev/null | sed '1d;$s|</pre>$||')"

if [ -n "$ok4_out" ]; then
  ok "OK4 fixture captured"
else
  bad "OK4 fixture produced no capture file"
fi

ok4_first="$(awk '/<pre>/{sub(/.*<pre>/,""); print; exit}' "$ok4_out" 2>/dev/null)"
if printf '%s' "$ok4_first" | grep -q '"aiTitle":"OK4 test title"'; then
  ok "title-slot value threaded through as adapter-meta.aiTitle"
else
  bad "aiTitle not threaded from title-slot"
fi

if printf '%s\n' "$ok4_body" | grep -q '\[intent\] Investigating the flaky test\\n'; then
  ok "tool_execution_start intent joined onto the matching toolResult"
else
  bad "intent join missing or misplaced"
fi

n_x="$(printf '%s\n' "$ok4_body" | grep -o 'x\{2000\}' | wc -l | tr -d ' ')"
if [ "${n_x:-0}" -ge 1 ]; then
  ok "toolResult text still capped at 2000 chars beneath the prepended intent"
else
  bad "toolResult cap broken by the intent prepend"
fi

if printf '%s\n' "$ok4_body" | grep -q '\[compaction\] flaky test + widget refactor'; then
  ok "compaction entry emitted as a synthetic assistant message"
else
  bad "compaction not translated"
fi
if printf '%s\n' "$ok4_body" | grep -q 'files read: /tmp/widget.py, /tmp/util.py' \
  && printf '%s\n' "$ok4_body" | grep -q 'files modified: /tmp/widget.py'; then
  ok "compaction message carries details.readFiles/modifiedFiles"
else
  bad "compaction file lists missing"
fi

if printf '%s\n' "$ok4_body" | grep -q '"type":"attachment"' \
  && printf '%s\n' "$ok4_body" | grep -q '"type":"hook_additional_context"' \
  && printf '%s\n' "$ok4_body" | grep -q '"hookEvent":"UserPromptSubmit"' \
  && printf '%s\n' "$ok4_body" | grep -q '&lt;!--kb-recall/1 kb=memory id=aaaaaaaaaaaa--&gt;'; then
  ok "kb.recall synthesized as a Claude-shaped hook_additional_context attachment"
else
  bad "recall-ledger attachment shape wrong"
fi

if printf '%s\n' "$ok4_body" | grep -q '\[session-exit\] kind=signal; pending tool calls: 1'; then
  ok "non-normal session_exit (with a pending call) emits a trailing synthetic message"
else
  bad "session_exit(signal) not emitted"
fi

# A clean/normal session_exit must emit nothing (no noise).
rm -f "$KB_SESSIONS_DIR"/session-*.html
NORMAL_FIXTURE="$SCRIPT_DIR/fixtures/omp-session-exit-normal.jsonl"
"$CAPTURE" "$NORMAL_FIXTURE" >/dev/null 2>&1
normal_out="$(find "$KB_SESSIONS_DIR" -name 'session-*-ok4normal1.html' | head -1)"
if [ -n "$normal_out" ] && ! grep -q '\[session-exit\]' "$normal_out"; then
  ok "clean session_exit (kind=normal, nothing pending) emits no noise"
else
  bad "clean session_exit unexpectedly emitted a marker"
fi

echo
echo "== OK4: subagent sidecar staging (real kb sessions capture engine) =="
# The sidecar digest walk is Rust-only (sessions_capture.rs) — the bash
# fallback the rest of this file forces never sees it, by design (see the
# header comment) — so this block shells out to the REAL installed kb.
if command -v kb >/dev/null 2>&1; then
  OK4_SUB_ROOT="$TMPROOT/subagents-e2e"
  mkdir -p "$OK4_SUB_ROOT/parent-session"
  cp "$SCRIPT_DIR/fixtures/omp-subagent.jsonl" \
    "$OK4_SUB_ROOT/parent-session/CliSurface.jsonl"
  cp "$SCRIPT_DIR/fixtures/omp-subagent.jsonl" \
    "$OK4_SUB_ROOT/parent-session/Web UI & Tests.jsonl"
  PARENT_SESSION="$OK4_SUB_ROOT/parent-session.jsonl"
  jq -nc '{type:"session",version:3,id:"parentsid1",timestamp:"2026-08-24T11:00:00.000Z",cwd:"/tmp/kb-omp-fixture"}' \
    >"$PARENT_SESSION"
  jq -nc '{type:"message",id:"p1",parentId:null,timestamp:"2026-08-24T11:00:01.000Z",message:{role:"user",content:[{type:"text",text:"delegate work"}]}}' \
    >>"$PARENT_SESSION"
  jq -nc '{type:"message",id:"p2",parentId:"p1",timestamp:"2026-08-24T11:00:02.000Z",message:{role:"assistant",model:"openrouter/stealth/ox-alpha",content:[{type:"text",text:"delegated"}]}}' \
    >>"$PARENT_SESSION"

  REAL_KB_SESSIONS_DIR="$TMPROOT/subagents-e2e-out"
  mkdir -p "$REAL_KB_SESSIONS_DIR"
  (
    PATH="$REAL_PATH"
    KB_SESSIONS_DIR="$REAL_KB_SESSIONS_DIR"
    export PATH KB_SESSIONS_DIR
    "$CAPTURE" "$PARENT_SESSION" >/dev/null 2>&1
  )
  sub_out="$(find "$REAL_KB_SESSIONS_DIR" -name 'session-*-parentsid1.html' | head -1)"

  if [ -n "$sub_out" ] && grep -q 'id="kb-session-subagents"' "$sub_out"; then
    ok "subagent sidecar dir staged + digested via the real kb sessions capture engine"
  else
    bad "subagent digest block missing from real-engine capture"
  fi
  if [ -n "$sub_out" ] && grep -q '"agent_id":"CliSurface"' "$sub_out" \
    && grep -q '"agent_id":"Web-UI---Tests"' "$sub_out"; then
    ok "both subagents captured; the space/ampersand name sanitized filesystem-safe"
  else
    bad "subagent name sanitization or multi-subagent count wrong"
  fi
else
  echo "skip - kb binary not on PATH, cannot exercise the real capture engine" >&2
fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
