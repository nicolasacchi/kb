#!/usr/bin/env bash
# test-slate-harvest.sh — SL5: kb-slate-harvest.sh, the grokclaude
# dispatcher's HARVEST adapter (design §12 "the dispatcher bridge").
#
# Hermetic: a fake `kb` on PATH that records every argv to a spy file and
# returns a canned `slate open --all --json` digest; a fixture job dir
# (meta.json + report.json) under a scratch tmp root. Real `jq`/`bash`.
#
# Runnable standalone:
#   bash plugins/kb-memory/hooks/tests/test-slate-harvest.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
HARVEST="$HOOKS_DIR/kb-slate-harvest.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-slate-harvest-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() {
  PASS=$((PASS + 1))
  printf 'ok      - %s\n' "$1"
}
bad() {
  FAIL=$((FAIL + 1))
  printf 'not ok  - %s\n' "$1"
}

mkdir -p "$TMPROOT/bin"
export PATH="$TMPROOT/bin:$PATH"

# Fake kb: records argv, serves a canned `slate open --all --json` digest
# naming a take on ref job:<JOB_TAKE_SEQ_FOR>, and otherwise succeeds
# unless KB_SLATE_FAIL=1.
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ -n "${KB_ARGV_SPY:-}" ]; then
  { printf 'kb'; printf ' %s' "$@"; printf '\n'; } >>"$KB_ARGV_SPY"
fi
if [ "${1:-}" = "slate" ] && [ "${2:-}" = "open" ]; then
  printf '%s' "${SLATE_OPEN_JSON:-{\"sections\":{\"take\":[]}}}"
  exit 0
fi
if [ "${1:-}" = "slate" ]; then
  [ "${KB_SLATE_FAIL:-0}" = "1" ] && exit 1
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"

mk_job() { # dir job_id session_id backend cwd
  local dir="$1" id="$2" sid="$3" backend="$4" cwd="$5"
  mkdir -p "$dir"
  jq -n --arg id "$id" --arg sid "$sid" --arg backend "$backend" --arg cwd "$cwd" \
    '{id: $id, session_id: $sid, backend: $backend, cwd: $cwd, findings_error: null}' \
    >"$dir/meta.json"
}

TAKE_DIGEST='{"sections":{"take":[{"seq":7,"kind":"take","refs":[{"raw":"job:job-abc123","display":"job-abc123","resolved":true}]}]}}'

echo "== kb-slate-harvest.sh test matrix =="
echo "tmp root: $TMPROOT"
echo

# --- 1. normal harvest: found (with ref) / idea (no-evidence found +
#        modeled) / ask (? appended) / tried (findings_error) / done -------
job1="$TMPROOT/job1"
mk_job "$job1" "job-abc123" "sess-abc" "kimi" "/tmp/proj"
jq -n '{ id: "job-abc123", session_id: "sess-abc", backend: "kimi", cwd: "/tmp/proj",
         findings_error: "worker never wrote report.json'"'"'s findings array" }' >"$job1/meta.json"
jq -n '{
  headlines: ["x"],
  findings: [
    {id:"f1", claim:"X does Y", claim_type:"observed", evidence:[{path:"src/x.rs", selector:".foo"}]},
    {id:"f2", claim:"no evidence here", claim_type:"observed", evidence:[]},
    {id:"f3", claim:"projected cost is high", claim_type:"modeled", evidence:[]}
  ],
  open_questions: ["does X do Y"]
}' >"$job1/report.json"

export KB_ARGV_SPY="$TMPROOT/spy1.txt"
export SLATE_OPEN_JSON="$TAKE_DIGEST"
bash "$HARVEST" "$job1"
unset KB_ARGV_SPY SLATE_OPEN_JSON
spy1="$(cat "$TMPROOT/spy1.txt" 2>/dev/null)"

case "$spy1" in
*'slate found X does Y --harness'*'--ref path:src/x.rs'*) ok "observed finding WITH evidence -> found --ref path:..." ;;
*) bad "observed finding WITH evidence -> found --ref path:... (got: $spy1)" ;;
esac
case "$spy1" in
*'slate idea no evidence here --harness'*) ok "observed finding with NO evidence path -> idea, never a bare found" ;;
*) bad "observed finding with NO evidence path -> idea (got: $spy1)" ;;
esac
case "$spy1" in
*'slate idea projected cost is high --harness'*) ok "modeled finding -> idea" ;;
*) bad "modeled finding -> idea (got: $spy1)" ;;
esac
case "$spy1" in
*'slate ask does X do Y? --harness'*) ok "open_questions entry -> ask with a trailing ? appended" ;;
*) bad "open_questions entry -> ask with ? appended (got: $spy1)" ;;
esac
case "$spy1" in
*'slate tried job job-abc123 findings error --harness'*'--failed'*) ok "meta.json findings_error -> tried --failed" ;;
*) bad "meta.json findings_error -> tried --failed (got: $spy1)" ;;
esac
case "$spy1" in
*'slate open --all --json --cwd /tmp/proj'*) ok "take lookup reads --cwd from meta.json" ;;
*) bad "take lookup reads --cwd from meta.json (got: $spy1)" ;;
esac
case "$spy1" in
*'slate done 7 job job-abc123 finished --harness'*) ok "resolved take seq 7 closed with a plain done (Finished, never re-live)" ;;
*) bad "resolved take seq 7 closed with a plain done (got: $spy1)" ;;
esac
case "$spy1" in
*'--abandoned'*) bad "normal harvest never passes --abandoned" ;;
*) ok "normal harvest never passes --abandoned" ;;
esac
case "$spy1" in
*'--harness kimi'*'--session-id sess-abc'*) ok "harness + session-id forwarded from meta.json" ;;
*) bad "harness + session-id forwarded from meta.json (got: $spy1)" ;;
esac

# --- 2. --abandoned mode: skips found/idea/ask/tried, only closes the take
job2="$TMPROOT/job2"
mk_job "$job2" "job-xyz789" "sess-xyz" "grok" "/tmp/proj2"
jq -n '{headlines:[], findings:[{id:"f1", claim:"should never post", claim_type:"observed",
        evidence:[{path:"x", selector:"."}]}], open_questions:["never posted?"]}' >"$job2/report.json"
export KB_ARGV_SPY="$TMPROOT/spy2.txt"
export SLATE_OPEN_JSON='{"sections":{"take":[{"seq":42,"kind":"take","refs":[{"raw":"job:job-xyz789"}]}]}}'
bash "$HARVEST" "$job2" --abandoned "reaped: stale"
unset KB_ARGV_SPY SLATE_OPEN_JSON
spy2="$(cat "$TMPROOT/spy2.txt" 2>/dev/null)"
case "$spy2" in
*'slate found'*|*'slate idea'*|*'slate ask'*|*'slate tried'*)
  bad "--abandoned mode never harvests found/idea/ask/tried (got: $spy2)" ;;
*) ok "--abandoned mode skips found/idea/ask/tried entirely" ;;
esac
case "$spy2" in
*'slate done 42 job job-xyz789: reaped: stale --abandoned reaped: stale'*)
  ok "--abandoned mode closes the resolved take with done --abandoned <reason>" ;;
*) bad "--abandoned mode closes with done --abandoned <reason> (got: $spy2)" ;;
esac

# --- 3. fake-* session -> completely silent, kb never invoked -------------
job3="$TMPROOT/job3"
mk_job "$job3" "job-fake1" "fake-worker-9" "grok" "/tmp/proj3"
export KB_ARGV_SPY="$TMPROOT/spy3.txt"
bash "$HARVEST" "$job3"
unset KB_ARGV_SPY
[ ! -s "$TMPROOT/spy3.txt" ] && ok "fake-* session id -> kb never invoked" \
  || bad "fake-* session id -> kb never invoked (spy: $(cat "$TMPROOT/spy3.txt"))"

# --- 4. missing job_dir -> silent no-op, never a non-zero exit ------------
export KB_ARGV_SPY="$TMPROOT/spy4.txt"
bash "$HARVEST" "$TMPROOT/does-not-exist"
rc4=$?
unset KB_ARGV_SPY
[ "$rc4" -eq 0 ] && ok "missing job_dir -> exit 0" || bad "missing job_dir -> exit 0 (rc=$rc4)"
[ ! -s "$TMPROOT/spy4.txt" ] && ok "missing job_dir -> kb never invoked" \
  || bad "missing job_dir -> kb never invoked"

# --- 5. job_dir present but no meta.json -> silent no-op ------------------
job5="$TMPROOT/job5"
mkdir -p "$job5"
export KB_ARGV_SPY="$TMPROOT/spy5.txt"
bash "$HARVEST" "$job5"
rc5=$?
unset KB_ARGV_SPY
[ "$rc5" -eq 0 ] && [ ! -s "$TMPROOT/spy5.txt" ] && ok "job_dir with no meta.json -> exit 0, kb never invoked" \
  || bad "job_dir with no meta.json -> exit 0, kb never invoked (rc=$rc5)"

# --- 6. no `kb` on PATH -> exit 0, never fails the caller ------------------
job6="$TMPROOT/job6"
mk_job "$job6" "job-nokb" "sess-nokb" "grok" "/tmp/proj6"
( PATH=/usr/bin:/bin bash "$HARVEST" "$job6" )
rc6=$?
[ "$rc6" -eq 0 ] && ok "no kb on PATH -> exit 0 (never fails the caller)" \
  || bad "no kb on PATH -> exit 0 (rc=$rc6)"

# --- 7. no live take found for job: ref -> no done call, still exit 0 -----
job7="$TMPROOT/job7"
mk_job "$job7" "job-noref" "sess-noref" "grok" "/tmp/proj7"
export KB_ARGV_SPY="$TMPROOT/spy7.txt"
export SLATE_OPEN_JSON='{"sections":{"take":[]}}'
bash "$HARVEST" "$job7"
rc7=$?
unset KB_ARGV_SPY SLATE_OPEN_JSON
spy7="$(cat "$TMPROOT/spy7.txt" 2>/dev/null)"
[ "$rc7" -eq 0 ] && ok "no take found for job: ref -> exit 0" || bad "no take found -> exit 0 (rc=$rc7)"
case "$spy7" in
*'slate done'*) bad "no take found for job: ref -> done is never called (got: $spy7)" ;;
*) ok "no take found for job: ref -> done is never called" ;;
esac

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
