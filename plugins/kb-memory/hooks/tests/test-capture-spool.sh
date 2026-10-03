#!/usr/bin/env bash
# test-capture-spool.sh — v0.44 X6: kb-capture.sh must NEVER embed a raw
# transcript in the corpus. When `kb sessions capture` fails (or `kb` is
# missing) the raw transcript goes to a private spool outside every corpus
# (dir 0700, files 0600); the next successful capture, or
# `kb sessions capture --replay-spool`, lands it through the scrubbed path and
# deletes it.
#
# Uses the REAL `kb` binary (KB_BIN_DIR, set by the cargo harness, else PATH)
# for the replay cases and a fake failing `kb` for the failure cases.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-capture-spool.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-capture-spool-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

GH="ghp_0123456789abcdefghijklmnopqrstuvwxyzAB"
REAL_PATH="$PATH"
[ -n "${KB_BIN_DIR:-}" ] && REAL_PATH="$KB_BIN_DIR:$PATH"

mkdir -p "$TMPROOT/failbin"
printf '#!/usr/bin/env bash\nexit 1\n' >"$TMPROOT/failbin/kb"
chmod +x "$TMPROOT/failbin/kb"

SID="sess-spool-0001"
TRANSCRIPT="$TMPROOT/t.jsonl"
cat >"$TRANSCRIPT" <<JSONL
{"sessionId":"$SID","type":"user","timestamp":"2026-03-01T09:00:00.000Z","message":{"role":"user","content":"my token is $GH ok"},"promptSource":"typed"}
JSONL
PAYLOAD="{\"session_id\":\"$SID\",\"transcript_path\":\"$TRANSCRIPT\",\"cwd\":\"$TMPROOT\"}"

export KB_CAPTURE_SPOOL="$TMPROOT/spool"
export KB_SESSIONS_DIR="$TMPROOT/sessions"
mkdir -p "$KB_SESSIONS_DIR"

hook() { # $1 = PATH
  printf '%s' "$PAYLOAD" | PATH="$1" bash "$HOOKS_DIR/kb-capture.sh" >/dev/null 2>&1
}

corpus_files() { find "$KB_SESSIONS_DIR" -type f 2>/dev/null | wc -l | tr -d ' '; }

echo "== a failing capture leaves nothing raw in the corpus =="
hook "$TMPROOT/failbin:$PATH"
[ "$(corpus_files)" = 0 ] && ok "failing kb: corpus is empty" || bad "failing kb: corpus is empty ($(corpus_files) file(s))"
if grep -rq "$GH" "$KB_SESSIONS_DIR" 2>/dev/null; then bad "failing kb: no raw secret in corpus"; else ok "failing kb: no raw secret in corpus"; fi
[ -f "$KB_CAPTURE_SPOOL/$SID.jsonl" ] && ok "failing kb: transcript parked in the spool" || bad "failing kb: transcript parked in the spool"
[ "$(stat -c %a "$KB_CAPTURE_SPOOL" 2>/dev/null)" = 700 ] && ok "spool dir is 0700" || bad "spool dir is 0700"
[ "$(stat -c %a "$KB_CAPTURE_SPOOL/$SID.jsonl" 2>/dev/null)" = 600 ] && ok "spool file is 0600" || bad "spool file is 0600"

echo "== kb missing entirely =="
rm -rf "$KB_CAPTURE_SPOOL"
JQ_DIR="$(dirname "$(command -v jq)")"
NOKB="$TMPROOT/nokb"; mkdir -p "$NOKB"; ln -sf "$(command -v jq)" "$NOKB/jq"
hook "$NOKB:/usr/bin:/bin"
[ "$(corpus_files)" = 0 ] && ok "no kb: corpus is empty" || bad "no kb: corpus is empty"
[ -f "$KB_CAPTURE_SPOOL/$SID.jsonl" ] && ok "no kb: transcript parked in the spool" || bad "no kb: transcript parked in the spool"
: "$JQ_DIR"

echo "== the next successful capture replays the spool, scrubbed =="
OTHER="$TMPROOT/t2.jsonl"
cat >"$OTHER" <<JSONL
{"sessionId":"sess-other-0002","type":"user","timestamp":"2026-03-01T10:00:00.000Z","message":{"role":"user","content":"hello"},"promptSource":"typed"}
JSONL
printf '%s' "{\"session_id\":\"sess-other-0002\",\"transcript_path\":\"$OTHER\",\"cwd\":\"$TMPROOT\"}" \
  | PATH="$REAL_PATH" bash "$HOOKS_DIR/kb-capture.sh" >/dev/null 2>&1
[ "$(corpus_files)" = 2 ] && ok "replay: both sessions landed" || bad "replay: both sessions landed ($(corpus_files) file(s))"
if grep -rq "$GH" "$KB_SESSIONS_DIR" 2>/dev/null; then bad "replay: no raw secret in corpus"; else ok "replay: no raw secret in corpus"; fi
grep -rq '\[redacted:' "$KB_SESSIONS_DIR" 2>/dev/null && ok "replay: redaction marker present" || bad "replay: redaction marker present"
[ -f "$KB_CAPTURE_SPOOL/$SID.jsonl" ] && bad "replay: spool item deleted" || ok "replay: spool item deleted"

echo "== explicit --replay-spool =="
rm -rf "$KB_SESSIONS_DIR" "$KB_CAPTURE_SPOOL"; mkdir -p "$KB_SESSIONS_DIR"
hook "$TMPROOT/failbin:$PATH"
PATH="$REAL_PATH" kb sessions capture --replay-spool --out "$KB_SESSIONS_DIR" >/dev/null 2>&1 \
  && ok "--replay-spool exits 0" || bad "--replay-spool exits 0"
[ "$(corpus_files)" = 1 ] && ok "--replay-spool: capture landed" || bad "--replay-spool: capture landed"
if grep -rq "$GH" "$KB_SESSIONS_DIR" 2>/dev/null; then bad "--replay-spool: scrubbed"; else ok "--replay-spool: scrubbed"; fi

echo "== a hung kb is bounded by the shared deadline (INT4) =="
mkdir -p "$TMPROOT/hangbin"
printf '#!/usr/bin/env bash\nexec sleep 30\n' >"$TMPROOT/hangbin/kb"
chmod +x "$TMPROOT/hangbin/kb"
rm -rf "$KB_SESSIONS_DIR" "$KB_CAPTURE_SPOOL"; mkdir -p "$KB_SESSIONS_DIR"
t0=$(date +%s)
printf '%s' "$PAYLOAD" | KB_CAPTURE_BUDGET_SECS=2 PATH="$TMPROOT/hangbin:$PATH" bash "$HOOKS_DIR/kb-capture.sh" >/dev/null 2>&1
el=$(( $(date +%s) - t0 ))
[ "$el" -lt 12 ] && ok "kb-capture.sh returns at its budget, not the 30s hang (${el}s)" || bad "kb-capture.sh waited out the hang (${el}s)"
[ -f "$KB_CAPTURE_SPOOL/$SID.jsonl" ] && ok "the hung capture is spooled, not lost" || bad "the hung capture is spooled, not lost"
[ "$(corpus_files)" = 0 ] && ok "nothing raw reached the corpus" || bad "nothing raw reached the corpus"

ROLLOUT="$TMPROOT/rollout.jsonl"
cat >"$ROLLOUT" <<JSONL
{"timestamp":"2026-03-01T09:00:00.000Z","type":"session_meta","payload":{"id":"codex-sess-0001","timestamp":"2026-03-01T09:00:00.000Z","cwd":"/tmp/x","originator":"codex","cli_version":"0"}}
{"timestamp":"2026-03-01T09:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}}
JSONL
t0=$(date +%s)
PATH="$TMPROOT/hangbin:$PATH" KB_CAPTURE_BUDGET_SECS=2 bash "$HOOKS_DIR/kb-capture-codex.sh" "$ROLLOUT" >/dev/null 2>&1
el=$(( $(date +%s) - t0 ))
[ "$el" -lt 12 ] && ok "kb-capture-codex.sh returns at its budget (${el}s)" || bad "kb-capture-codex.sh waited out the hang (${el}s)"
[ "$(corpus_files)" = 0 ] && ok "codex: a hung scrubber fails closed (nothing written)" || bad "codex: a hung scrubber fails closed"

printf '\n%s passed, %s failed\n' "$PASS" "$FAIL"
[ "$FAIL" = 0 ]
