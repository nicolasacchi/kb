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
# AH1: sandbox HOME/XDG/KB_* before anything can reach a kb (see lib-sandbox.sh).
. "$SCRIPT_DIR/lib-sandbox.sh"
kb_test_sandbox "$TMPROOT" || exit 1

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

mkdir -p "$TMPROOT/bin"
cp "$SCRIPT_DIR/fixtures/fake-capture-kb.sh" "$TMPROOT/bin/kb"
chmod +x "$TMPROOT/bin/kb"
export HOOKS_DIR
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
# AH1 drain path: a capture that lands here makes hook_adapter_land run
# `kb sessions capture --replay-spool` against the DEFAULT spool, landing (and
# deleting) every parked item into $REAL_KB_SESSIONS_DIR, which the EXIT trap
# then removes with TMPROOT. The kb_test_sandbox call near the top of this file
# is what keeps that default spool inside TMPROOT; it must stay above every
# real-kb line.
# The sidecar digest walk is Rust-only (sessions_capture.rs) — the bash
# fallback the rest of this file forces never sees it, by design (see the
# header comment) — so this block shells out to the REAL installed kb.
# `command -v kb` here would find the stand-in installed above, so look for the
# real one on the pre-override PATH or in KB_BIN_DIR (what CI sets).
if PATH="$REAL_PATH" command -v kb >/dev/null 2>&1 \
  || [ -n "${KB_BIN_DIR:-}" ] && [ -x "${KB_BIN_DIR:-}/kb" ]; then
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
  echo "skip - no real kb on PATH or in KB_BIN_DIR, cannot exercise the real capture engine" >&2
fi


echo
echo "== N10: sidecar agent names (collision-only disambiguator) + spooled sidecars =="
N10_ROOT="$TMPROOT/n10"
mkdir -p "$N10_ROOT/parent-session" "$N10_ROOT/bin" "$N10_ROOT/rec" "$N10_ROOT/spool"
cp "$SCRIPT_DIR/fixtures/omp-subagent.jsonl" "$N10_ROOT/parent-session/Web UI & Tests.jsonl"
cp "$SCRIPT_DIR/fixtures/omp-subagent.jsonl" "$N10_ROOT/parent-session/Web-UI---Tests.jsonl"
cp "$SCRIPT_DIR/fixtures/omp-subagent.jsonl" "$N10_ROOT/parent-session/Plain.jsonl"
N10_SESSION="$N10_ROOT/parent-session.jsonl"
jq -nc '{type:"session",version:3,id:"n10sid1",timestamp:"2026-08-24T11:00:00.000Z",cwd:"/tmp/kb-omp-fixture"}' >"$N10_SESSION"
jq -nc '{type:"message",id:"p1",parentId:null,timestamp:"2026-08-24T11:00:01.000Z",message:{role:"user",content:[{type:"text",text:"delegate"}]}}' >>"$N10_SESSION"
# A kb that records the staged scratch tree it was handed (or fails, per env).
cat >"$N10_ROOT/bin/kb" <<'KB'
#!/usr/bin/env bash
[ "${N10_KB_FAIL:-}" = 1 ] && exit 1
while [ $# -gt 0 ]; do
  [ "$1" = --transcript ] && { t="$2"; break; }
  shift
done
[ -n "${t:-}" ] && cp -R "$(dirname "$t")/." "$N10_REC/"
exit 0
KB
chmod +x "$N10_ROOT/bin/kb"
export N10_REC="$N10_ROOT/rec"
(
  export PATH="$N10_ROOT/bin:$PATH" KB_SESSIONS_DIR="$N10_ROOT/out" KB_CAPTURE_SPOOL="$N10_ROOT/spool"
  mkdir -p "$KB_SESSIONS_DIR"
  "$CAPTURE" "$N10_SESSION" >/dev/null 2>&1
)
agents="$(ls "$N10_ROOT/rec/n10sid1/subagents" 2>/dev/null | grep '^agent-.*\.jsonl$' | sort)"
[ "$(printf '%s\n' "$agents" | wc -l | tr -d ' ')" = 3 ] \
  && ok "three agents with colliding safe names stay three distinct files" \
  || bad "agent files collided or went missing: $agents"
printf '%s\n' "$agents" | grep -qx 'agent-Plain.jsonl' \
  && ok "a non-colliding agent keeps its readable name" || bad "readable name changed: $agents"
printf '%s\n' "$agents" | grep -qx 'agent-Web-UI---Tests.jsonl' \
  && ok "the first of two colliding ids keeps the readable name" || bad "first colliding id lost its readable name: $agents"
printf '%s\n' "$agents" | grep -qE '^agent-Web-UI---Tests-[0-9a-f]{8}\.jsonl$' \
  && ok "the second colliding id gets a short-hash disambiguator" || bad "second colliding id not disambiguated: $agents"

# Failed capture: the translated sidecars ride into the spool next to the main item.
(
  export PATH="$N10_ROOT/bin:$PATH" KB_SESSIONS_DIR="$N10_ROOT/out2" KB_CAPTURE_SPOOL="$N10_ROOT/spool" N10_KB_FAIL=1
  mkdir -p "$KB_SESSIONS_DIR"
  "$CAPTURE" "$N10_SESSION" >/dev/null 2>&1
)
if [ -s "$N10_ROOT/spool/n10sid1.jsonl" ] && ls "$N10_ROOT/spool/n10sid1/subagents"/agent-*.jsonl >/dev/null 2>&1; then
  ok "failed capture spools the main transcript AND its translated sidecars"
else
  bad "spool lacks main transcript or sidecars: $(find "$N10_ROOT/spool" | tr '\n' ' ')"
fi

echo
echo "== A1: adapter-meta job_ulid from KB_JOB_ULID =="
job1_out="$TMPROOT/sessions-job1"
job2_out="$TMPROOT/sessions-job2"
job3_out="$TMPROOT/sessions-job3"
mkdir -p "$job1_out" "$job2_out" "$job3_out"
KB_SESSIONS_DIR="$job1_out" KB_JOB_ULID=01FIXTUREJOBULID0000000A "$CAPTURE" "$SESSION" >/dev/null 2>&1
KB_SESSIONS_DIR="$job2_out" env -u KB_JOB_ULID "$CAPTURE" "$SESSION" >/dev/null 2>&1
KB_SESSIONS_DIR="$job3_out" KB_JOB_ULID='a b;rm' "$CAPTURE" "$SESSION" >/dev/null 2>&1
job1_first="$(awk '/<pre>/{sub(/.*<pre>/,""); print; exit}' "$(find "$job1_out" -name "session-*-$SAFE_SID.html" | head -1)" 2>/dev/null)"
job2_first="$(awk '/<pre>/{sub(/.*<pre>/,""); print; exit}' "$(find "$job2_out" -name "session-*-$SAFE_SID.html" | head -1)" 2>/dev/null)"
job3_first="$(awk '/<pre>/{sub(/.*<pre>/,""); print; exit}' "$(find "$job3_out" -name "session-*-$SAFE_SID.html" | head -1)" 2>/dev/null)"
if printf '%s' "$job1_first" | jq -e '.job_ulid == "01FIXTUREJOBULID0000000A"' >/dev/null 2>&1; then
  ok "valid KB_JOB_ULID stamps adapter-meta job_ulid"
else
  bad "valid KB_JOB_ULID did not stamp job_ulid: $job1_first"
fi
if printf '%s' "$job2_first" | jq -e 'has("job_ulid") == false' >/dev/null 2>&1; then
  ok "unset KB_JOB_ULID omits job_ulid"
else
  bad "unset KB_JOB_ULID still carries job_ulid: $job2_first"
fi
if printf '%s' "$job3_first" | jq -e 'has("job_ulid") == false' >/dev/null 2>&1; then
  ok "invalid KB_JOB_ULID omits job_ulid"
else
  bad "invalid KB_JOB_ULID still carries job_ulid: $job3_first"
fi
echo
echo "== A2: session lineage from the shared fixture =="
# Generate a synthetic tree from session_lineage_cases.json; each file's
# header carries id/parentSession so cap_lineage folds the same (parent,
# root) the Rust fold_lineage expects.
LIN_FIXTURE="$HOOKS_DIR/../../../crates/kb-core/tests/fixtures/session_lineage_cases.json"
LIN_ROOT="$TMPROOT/lintree"
mkdir -p "$LIN_ROOT"
LIN_CASES_FILE="$TMPROOT/lin-cases.txt"
# Each case gets its OWN tree (case-<i>/bucket/<rel>) so the sibling of one
# nested case can never satisfy the no-sibling case in the same dir.
if [ -f "$LIN_FIXTURE" ]; then
  python3 -I - "$LIN_FIXTURE" "$LIN_ROOT" >"$LIN_CASES_FILE" <<'PYEOF'
import json, sys, os
cases = json.load(open(sys.argv[1]))
root = sys.argv[2]
msg = {"type": "message", "id": "m1", "parentId": None, "timestamp": "2026-09-01T10:00:01.000Z", "message": {"role": "user", "content": [{"type": "text", "text": "hi"}]}}
for i, c in enumerate(cases):
    base = os.path.join(root, "case-%d" % i, "bucket")
    sf = c["session_file"]
    rel = sf.split("/bucket/")[-1] if "/bucket/" in sf else sf
    target = os.path.join(base, rel)
    os.makedirs(os.path.dirname(target), exist_ok=True)
    hdr = {"type": "session", "id": c["header_id"], "timestamp": "2026-09-01T10:00:00.000Z", "cwd": "/tmp/kb-omp-fixture"}
    ps = c.get("parent_session_file")
    if ps:
        # point the header parentSession at the mapped parent path
        prel = ps.split("/bucket/")[-1] if "/bucket/" in ps else ps
        hdr["parentSession"] = os.path.join(base, prel)
    with open(target, "w") as f:
        f.write(json.dumps(hdr) + "\n")
        f.write(json.dumps(msg) + "\n")
    # materialize the referenced parent file in this tree so the Oct
    # parent-header resolution path is exercised (skip when the case
    # expects an unreadable parent or is its own parent).
    if ps and c.get("parent_header_id") and ps.split("/")[-1] != rel:
        ppath = os.path.join(base, prel)
        if not os.path.exists(ppath):
            phdr = {"type": "session", "id": c["parent_header_id"], "timestamp": "2026-09-01T10:00:00.000Z", "cwd": "/tmp/kb-omp-fixture"}
            os.makedirs(os.path.dirname(ppath), exist_ok=True)
            with open(ppath, "w") as f:
                f.write(json.dumps(phdr) + "\n")
                f.write(json.dumps(msg) + "\n")
    with open(target, "w") as f:
        f.write(json.dumps(hdr) + "\n")
        f.write(json.dumps(msg) + "\n")
    # sibling <dir>/<Name>.jsonl for nested cases with a sibling header:
    # the parent session file lives around the <Name> dir (brief step 4).
    if c.get("parent_header_id") and not c.get("parent_session_file"):
        parts = rel.split("/")
        if len(parts) >= 3:
            name = parts[-2]
            sib = os.path.join(base, parts[0], name + ".jsonl")
            if not os.path.exists(sib):
                shdr = {"type": "session", "id": c["parent_header_id"], "timestamp": "2026-09-01T10:00:00.000Z", "cwd": "/tmp/kb-omp-fixture"}
                with open(sib, "w") as f:
                    f.write(json.dumps(shdr) + "\n")
                    f.write(json.dumps(msg) + "\n")
    # one 4-line record per case: path, header id, expect parent, expect root
    print(target)
    print(c["header_id"])
    print(c["expect"]["parent"] or "")
    print(c["expect"]["root"] or "")
PYEOF
fi
# Extract cap_is_plain_token + cap_lineage WITHOUT running the script's
# main block: kb-capture-omp.sh has no source guard (sourcing runs the
# capture and exits 0), so slice the two functions into a lib and source
# only that. The awk stops at the closing `}` of cap_lineage (verified:
# no other line matches `^}$` inside it).
LIN_LIB="$TMPROOT/caplineage-lib.sh"
awk '/^cap_is_plain_token\(\)/ {f=1} f {print} /^cap_lineage\(\)/ {g=1} g && /^\}$/ {exit}' "$CAPTURE" >"$LIN_LIB"
# Directly assert cap_lineage output per shared case (4 lines per case:
# path, header id, expect parent, expect root). Line-oriented reads keep
# the unsafe-root-id "bad id!" dir (a space in the path) in one piece.
if [ -s "$LIN_CASES_FILE" ]; then
while IFS= read -r LIN_F && IFS= read -r LIN_ID && IFS= read -r LIN_P && IFS= read -r LIN_R; do
  LIN_OUT="$(bash -c '. "$1"; cap_lineage "$2"' _ "$LIN_LIB" "$LIN_F" 2>/dev/null || true)"
  LIN_OK=1
  if [ -n "$LIN_P" ]; then
    printf '%s' "$LIN_OUT" | jq -e --arg v "$LIN_P" '.parentSessionId == $v' >/dev/null 2>&1 || LIN_OK=0
  else
    printf '%s' "$LIN_OUT" | jq -e 'has("parentSessionId") == false' >/dev/null 2>&1 || LIN_OK=0
  fi
  if [ -n "$LIN_R" ]; then
    printf '%s' "$LIN_OUT" | jq -e --arg v "$LIN_R" '.rootSessionId == $v' >/dev/null 2>&1 || LIN_OK=0
  else
    printf '%s' "$LIN_OUT" | jq -e 'has("rootSessionId") == false' >/dev/null 2>&1 || LIN_OK=0
  fi
  if [ "$LIN_OK" = "1" ]; then
    ok "cap_lineage $LIN_ID -> parent=${LIN_P:-none} root=${LIN_R:-none}"
  else
    bad "cap_lineage $LIN_ID: got $LIN_OUT want parent=${LIN_P:-none} root=${LIN_R:-none}"
  fi
done <"$LIN_CASES_FILE"
else
  bad "shared lineage fixture missing at $LIN_FIXTURE"
fi

echo
echo "== A2: cap_lineage splits ancestor dirs literally (no glob expansion) =="
# A stem-shaped dir whose prefix carries a glob char is not a <ts> stem
# (its prefix is not timestamp-shaped). With pathname expansion on, the
# `202?-...` word would expand to the sibling `2022-...` stem and
# misattribute the root. With `set -f` the split is literal: {}.
GLOB_ROOT="$TMPROOT/globlineage"
mkdir -p "$GLOB_ROOT/202?-09-01T10-00-00-000Z_roootG"
mkdir -p "$GLOB_ROOT/2022-09-01T10-00-00-000Z_roootG"
printf '%s\n%s\n' '{"type":"session","id":"childG001","timestamp":"2026-09-01T10:00:00.000Z","cwd":"/tmp/kb-omp-fixture"}' '{"type":"message","id":"m1","timestamp":"2026-09-01T10:00:01.000Z","message":{"role":"user","content":[{"type":"text","text":"hi"}]}}' > "$GLOB_ROOT/202?-09-01T10-00-00-000Z_roootG/childG001.jsonl"
GLOB_OUT="$(cd "$GLOB_ROOT" && bash -c '. "$1"; cap_lineage "$2"' _ "$LIN_LIB" "$GLOB_ROOT/202?-09-01T10-00-00-000Z_roootG/childG001.jsonl" 2>/dev/null || true)"
if printf '%s' "$GLOB_OUT" | jq -e 'keys == []' >/dev/null 2>&1; then
  ok "cap_lineage splits glob-char dirs literally (no root from '?')"
else
  bad "cap_lineage glob-expanded an ancestor dir: $GLOB_OUT (want {})"
fi
# Static wiring pin: --argjson lineage reaches exactly the main transcript
# and both segment (P1/PN) translations; the sidecar call takes none.
LIN_NARG="$(grep -c -- '--argjson lineage' "$CAPTURE")"
if [ "$LIN_NARG" = "3" ]; then
  ok "lineage argjson on the main + both segment translations only (3 sites)"
else
  bad "lineage argjson site count is $LIN_NARG, want 3 (main + P1 + PN)"
fi
# A top-level capture still lands and carries neither lineage key.
LIN_OUTDIR="$TMPROOT/sessions-lineage"
mkdir -p "$LIN_OUTDIR"
KB_SESSIONS_DIR="$LIN_OUTDIR" "$CAPTURE" "$LIN_ROOT/case-0/bucket/2026-09-01T10-00-00-000Z_root0001.jsonl" >/dev/null 2>&1
LIN_FIRST="$(awk '/<pre>/{sub(/.*<pre>/,""); print; exit}' "$(find "$LIN_OUTDIR" -name "session-*-root0001.html" | head -1)" 2>/dev/null)"
if printf '%s' "$LIN_FIRST" | jq -e 'has("parentSessionId") == false and has("rootSessionId") == false' >/dev/null 2>&1; then
  ok "top-level capture carries neither lineage key"
else
  bad "top-level capture lineage keys wrong: $LIN_FIRST"
fi
# A child capture carries both.
LIN_OUTDIR2="$TMPROOT/sessions-lineage2"
mkdir -p "$LIN_OUTDIR2"
KB_SESSIONS_DIR="$LIN_OUTDIR2" "$CAPTURE" "$LIN_ROOT/case-1/bucket/2026-09-01T10-00-00-000Z_root0001/child0001.jsonl" >/dev/null 2>&1
LIN_FIRST2="$(awk '/<pre>/{sub(/.*<pre>/,""); print; exit}' "$(find "$LIN_OUTDIR2" -name "session-*-child0001.html" | head -1)" 2>/dev/null)"
if printf '%s' "$LIN_FIRST2" | jq -e '.parentSessionId == "root0001" and .rootSessionId == "root0001"' >/dev/null 2>&1; then
  ok "child capture stamps parent/root lineage"
else
  bad "child capture lineage wrong: $LIN_FIRST2"
fi
# Sidecar translations still succeed alongside lineage capture: a failing
# kb parks the translated main transcript (carrying lineage) AND the
# staged sidecar (translated with no lineage arg) in the spool.
LIN_SPOOLD="$TMPROOT/linspool"
LIN_BIN="$TMPROOT/linbin"
mkdir -p "$LIN_SPOOLD" "$LIN_BIN"
mkdir -p "$LIN_ROOT/case-1/bucket/2026-09-01T10-00-00-000Z_root0001/child0001"
jq -nc '{type:"session",version:3,id:"side0001",timestamp:"2026-09-01T10:00:00.000Z",cwd:"/tmp/kb-omp-fixture"}' >"$LIN_ROOT/case-1/bucket/2026-09-01T10-00-00-000Z_root0001/child0001/Worker.jsonl"
jq -nc '{type:"message",id:"sm1",parentId:null,timestamp:"2026-09-01T10:00:01.000Z",message:{role:"user",content:[{type:"text",text:"side work"}]}}' >>"$LIN_ROOT/case-1/bucket/2026-09-01T10-00-00-000Z_root0001/child0001/Worker.jsonl"
cat >"$LIN_BIN/kb" <<'KB'
#!/usr/bin/env bash
[ "${LIN_KB_FAIL:-}" = 1 ] && exit 1
exit 0
KB
chmod +x "$LIN_BIN/kb"
LIN_SIDE_OUT="$TMPROOT/linside-out"
mkdir -p "$LIN_SIDE_OUT"
(
  export PATH="$LIN_BIN:$PATH" KB_SESSIONS_DIR="$LIN_SIDE_OUT" KB_CAPTURE_SPOOL="$LIN_SPOOLD" LIN_KB_FAIL=1
  "$CAPTURE" "$LIN_ROOT/case-1/bucket/2026-09-01T10-00-00-000Z_root0001/child0001.jsonl" >/dev/null 2>&1
)
LIN_SPOOL_MAIN="$(head -1 "$LIN_SPOOLD/child0001.jsonl" 2>/dev/null)"
if printf '%s' "$LIN_SPOOL_MAIN" | jq -e '.parentSessionId == "root0001" and .rootSessionId == "root0001"' >/dev/null 2>&1 \
  && [ -s "$LIN_SPOOLD/child0001.jsonl" ] && ls "$LIN_SPOOLD/child0001/subagents"/agent-*.jsonl >/dev/null 2>&1; then
  ok "sidecar translation succeeds with lineage (spooled main carries it, sidecar staged)"
else
  bad "sidecar/lineage spool wrong: main=$LIN_SPOOL_MAIN spool=$(find "$LIN_SPOOLD" 2>/dev/null | tr '\n' ' ')"
fi
# Segmented capture stamps lineage on EVERY part: force a small segment
# target with the real kb so the child file lands as several parts.
if PATH="$REAL_PATH" command -v kb >/dev/null 2>&1 \
  || [ -n "${KB_BIN_DIR:-}" ] && [ -x "${KB_BIN_DIR:-}/kb" ]; then
  LIN_SEG_ROOT="$TMPROOT/linseg"
  mkdir -p "$LIN_SEG_ROOT/bucket/2026-09-01T10-00-00-000Z_root0001"
  LIN_SEG_FILE="$LIN_SEG_ROOT/bucket/2026-09-01T10-00-00-000Z_root0001/segchild.jsonl"
  {
    jq -nc '{type:"session",version:3,id:"segchild",timestamp:"2026-09-01T10:00:00.000Z",cwd:"/tmp/kb-omp-fixture"}'
    jq -nc '{type:"message",id:"m1",parentId:null,timestamp:"2026-09-01T10:00:01.000Z",message:{role:"user",content:[{type:"text",text:"turn one message body"}]}}'
    jq -nc '{type:"message",id:"m2",parentId:"m1",timestamp:"2026-09-01T10:00:02.000Z",message:{role:"assistant",model:"m",content:[{type:"text",text:"turn one answer body"}]}}'
    jq -nc '{type:"message",id:"m3",parentId:"m2",timestamp:"2026-09-01T10:00:03.000Z",message:{role:"user",content:[{type:"text",text:"turn two message body"}]}}'
    jq -nc '{type:"message",id:"m4",parentId:"m3",timestamp:"2026-09-01T10:00:04.000Z",message:{role:"assistant",model:"m",content:[{type:"text",text:"turn two answer body"}]}}'
    jq -nc '{type:"message",id:"m5",parentId:"m4",timestamp:"2026-09-01T10:00:05.000Z",message:{role:"user",content:[{type:"text",text:"turn three message body"}]}}'
    jq -nc '{type:"message",id:"m6",parentId:"m5",timestamp:"2026-09-01T10:00:06.000Z",message:{role:"assistant",model:"m",content:[{type:"text",text:"turn three answer body"}]}}'
  } >"$LIN_SEG_FILE"
  LIN_SEG_OUT="$TMPROOT/linseg-out"
  mkdir -p "$LIN_SEG_OUT"
  (
    PATH="$REAL_PATH"
    KB_SESSIONS_DIR="$LIN_SEG_OUT"
    KB_CAPTURE_SEGMENTS=1
    KB_CAPTURE_SEGMENT_BYTES=300
    export PATH KB_SESSIONS_DIR KB_CAPTURE_SEGMENTS KB_CAPTURE_SEGMENT_BYTES
    "$CAPTURE" "$LIN_SEG_FILE" >/dev/null 2>&1
  )
  LIN_SEG_FILES="$(find "$LIN_SEG_OUT" -name 'session-*-segchild*.html' 2>/dev/null)"
  LIN_SEG_N="$(printf '%s\n' "$LIN_SEG_FILES" | grep -c . || true)"
  LIN_SEG_OK=1
  [ "$LIN_SEG_N" -ge 2 ] || LIN_SEG_OK=0
  for part in $LIN_SEG_FILES; do
    part_first="$(awk '/<pre>/{sub(/.*<pre>/,""); print; exit}' "$part" 2>/dev/null)"
    printf '%s' "$part_first" | jq -e '.parentSessionId == "root0001" and .rootSessionId == "root0001"' >/dev/null 2>&1 || LIN_SEG_OK=0
  done
  if [ "$LIN_SEG_OK" = "1" ]; then
    ok "segmented capture stamps parent/root lineage on every part ($LIN_SEG_N parts)"
  else
    bad "segmented lineage wrong or too few parts: n=$LIN_SEG_N files=$LIN_SEG_FILES"
  fi
else
  echo "skip - no real kb on PATH or in KB_BIN_DIR, cannot exercise the segmented lineage run" >&2
fi

echo
echo "== T20: omp images: user + toolResult images become blob_ref blocks =="
IMG_SESSION="$TMPROOT/images.jsonl"
{
  title='{"type":"title","v":1,"title":"images test"}'
  pad_len=$((256 - ${#title} - 1))
  printf '%s%*s\n' "$title" "$pad_len" ''
  cat "$SCRIPT_DIR/fixtures/omp-session-images.jsonl"
} >"$IMG_SESSION"
IMG_OUTDIR="$TMPROOT/sessions-images"
mkdir -p "$IMG_OUTDIR"
KB_SESSIONS_DIR="$IMG_OUTDIR" "$CAPTURE" "$IMG_SESSION" >/dev/null 2>&1
img_out="$(find "$IMG_OUTDIR" -name 'session-*-imgsess1.html' | head -1)"
img_body="$(sed -n '/<pre>/,/<\/pre>/p' "$img_out" 2>/dev/null | sed '1d;$s|</pre>$||')"
if [ -n "$img_out" ]; then
  ok "images fixture captured"
else
  bad "images fixture produced no capture file"
fi
if printf '%s\n' "$img_body" | jq -e 'select(.type == "user" and (.message.content | map(select(.type == "image")) | length) == 1) | .message.content[1] | select(.source.type == "blob_ref" and .source.media_type == "image/png")' >/dev/null 2>&1; then
  ok "user message carries a blob_ref image block"
else
  bad "user image blob_ref missing: $img_body"
fi
if printf '%s\n' "$img_body" | jq -e 'select(.message.content | map(select(.type == "tool_result")) | length == 1) | .message.content[0].content | map(select(.type == "image")) | length == 1' >/dev/null 2>&1; then
  ok "tool_result keeps its text entry plus an image block"
else
  bad "tool_result image missing: $img_body"
fi
img_sha="$(printf '%s\n' "$img_body" | jq -r 'select(.type == "user") | .message.content[1].source.sha256 // empty' | head -1)"
if [ "$img_sha" = "abababababababababababababababababababababababababababababababab" ]; then
  ok "user image carries the invented blob sha"
else
  bad "user image sha wrong: $img_sha"
fi
if printf '%s\n' "$img_body" | jq -e '[.. | objects | select(.type == "image")] | map(select(has("data"))) | length == 0' >/dev/null 2>&1; then
  ok "no image block in the capture keeps a data key"
else
  bad "an image block kept its data key"
fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
