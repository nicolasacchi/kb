#!/usr/bin/env bash
# test-distill-nudge-omp.sh — self-contained test matrix for
# kb-distill-nudge-omp.sh, reading the Oh My Pi session JSONL encoding
# (session header / message entries / toolCall arguments.command). Runs the
# REAL script against the checked-in omp-session-{commit,remembered}.jsonl
# fixtures: a commit-without-remembered session fires once (marker-gated)
# and appends to the shared distill-pending ledger; a remembered session is
# suppressed (MI-W0.3 success-aware suppression); no-commit sessions are
# silent no-ops.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-distill-nudge-omp.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
NUDGE="$HOOKS_DIR/kb-distill-nudge-omp.sh"
COMMIT_FIXTURE="$SCRIPT_DIR/fixtures/omp-session-commit.jsonl"
REMEMBERED_FIXTURE="$SCRIPT_DIR/fixtures/omp-session-remembered.jsonl"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-nudge-omp-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

export KB_SESSIONS_DIR="$TMPROOT/sessions"   # same gate every capture hook uses
export XDG_CACHE_HOME="$TMPROOT/cache"
mkdir -p "$KB_SESSIONS_DIR"

SID="fx01a034-0000-0000-0000-000000000001"

echo "== kb-distill-nudge-omp.sh test matrix =="
echo "tmp root: $TMPROOT"
echo

# --- 1. commit without a successful remember -> fires, appends ledger ------
out="$(printf '{"session_file":"%s","session_id":"%s","cwd":"/tmp/kb-omp-fixture"}\n' \
  "$COMMIT_FIXTURE" "$SID" | "$NUDGE" 2>/dev/null)"
if printf '%s' "$out" | grep -q 'committed code but kept no curated memory'; then
  ok "commit-without-remember prints the one-line nudge"
else
  bad "expected nudge output, got: [$out]"
fi

ledger="$XDG_CACHE_HOME/kb/distill-pending"
line="$(tail -1 "$ledger" 2>/dev/null || true)"
if printf '%s' "$line" | grep -qE "^omp $SID [0-9]+$"; then
  ok "ledger line appended as 'omp <sid> <epoch>'"
else
  bad "ledger line wrong: [$line]"
fi

if [ -e "$XDG_CACHE_HOME/kb/distill-nudged-omp-$SID" ]; then
  ok "once-per-session marker written"
else
  bad "marker missing"
fi

# --- 2. second run for the same session -> silent (marker gate) ------------
out2="$(printf '{"session_file":"%s","session_id":"%s"}\n' "$COMMIT_FIXTURE" "$SID" \
  | "$NUDGE" 2>/dev/null)"
lines="$(wc -l <"$ledger" | tr -d ' ')"
if [ -z "$out2" ] && [ "$lines" = "1" ]; then
  ok "second run is a silent no-op (no duplicate ledger entry)"
else
  bad "second run not suppressed (out=[$out2] lines=$lines)"
fi

# --- 3. remembered marker present -> suppressed -----------------------------
sid2="fx01a034-0000-0000-0000-000000000002"
out3="$(printf '{"session_file":"%s","session_id":"%s"}\n' "$REMEMBERED_FIXTURE" "$sid2" \
  | "$NUDGE" 2>/dev/null)"
if [ -z "$out3" ] && [ "$(wc -l <"$ledger" | tr -d ' ')" = "1" ]; then
  ok "successful 'remembered <12-hex>' suppresses the nudge"
else
  bad "remembered fixture was not suppressed (out=[$out3])"
fi

# --- 4. no git commit at all -> silent --------------------------------------
sed 's/git commit -m \\"fix: repair the widget\\"/echo nothing to see/' \
  "$COMMIT_FIXTURE" >"$TMPROOT/nocommit.jsonl"
sid4="fx01a034-0000-0000-0000-000000000004"
out4="$(printf '{"session_file":"%s","session_id":"%s"}\n' "$TMPROOT/nocommit.jsonl" "$sid4" \
  | "$NUDGE" 2>/dev/null)"
if [ -z "$out4" ] && ! grep -q "$sid4" "$ledger" 2>/dev/null; then
  ok "session without git commits stays silent"
else
  bad "no-commit session fired (out=[$out4])"
fi

# --- 5. CLI mode derives sid from the session header ------------------------
rm -f "$XDG_CACHE_HOME/kb/distill-nudged-omp-$SID"
out5="$("$NUDGE" "$COMMIT_FIXTURE" 2>/dev/null)"
if printf '%s' "$out5" | grep -q 'committed code but kept no curated memory' \
   && tail -1 "$ledger" | grep -q "^omp $SID "; then
  ok "CLI mode reads sid from the file's own header"
else
  bad "CLI mode failed (out=[$out5])"
fi

# --- 6. missing file / empty stdin -> exit 0, silent ------------------------
printf '{}\n' | "$NUDGE" >/dev/null 2>&1; rc6=$?
"$NUDGE" /nonexistent/file.jsonl >/dev/null 2>&1; rc7=$?
if [ "$rc6" = "0" ] && [ "$rc7" = "0" ]; then
  ok "missing payload fields / nonexistent file still exit 0"
else
  bad "non-silent failure paths (rc=$rc6/$rc7)"
fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
