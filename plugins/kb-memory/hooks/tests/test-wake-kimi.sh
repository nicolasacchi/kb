#!/usr/bin/env bash
# test-wake-kimi.sh — self-contained test matrix for kb-wake-kimi.sh: the
# Kimi Code UserPromptSubmit wake hook (memory protocol + recent-memories
# index + distill-pending surfacing on the FIRST prompt of a session —
# Kimi's SessionStart stdout never reaches the model, verified by a live
# hook probe). Runs the REAL script against mktemp caches with a fake
# `kb` on PATH (only `kb recall` is exercised). `jq` is real.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-wake-kimi.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WAKE="$HOOKS_DIR/kb-wake-kimi.sh"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-wake-kimi-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

mkdir -p "$TMPROOT/bin"
cat >"$TMPROOT/bin/kb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "recall" ]; then
  echo '{"hits":[{"title":"T1","kb":"main","summary":"a memory"}]}'
  exit 0
fi
exit 0
EOF
chmod +x "$TMPROOT/bin/kb"
export PATH="$TMPROOT/bin:$PATH"

SID="session_wakekimi-1111-2222"
MARKER_NAME="waked-kimi-session-wakekimi-1111-2222"

run_wake() {
  local cache="$1"
  export XDG_CACHE_HOME="$cache"
  mkdir -p "$cache"
  printf '%s' "{\"session_id\":\"$SID\",\"cwd\":\"/tmp\",\"hook_event_name\":\"UserPromptSubmit\"}" | "$WAKE"
}

echo "== kb-wake-kimi.sh test matrix =="
echo "tmp root: $TMPROOT"
echo

# --- 1. first prompt: protocol + index + kimi ledger entry surfaced,
#        ledger consumed, marker created --------------------------------
CACHE1="$TMPROOT/cache1"
mkdir -p "$CACHE1/kb"
printf 'kimi %s %s\n' "$SID" "$(date +%s)" >"$CACHE1/kb/distill-pending"
out="$(run_wake "$CACHE1")"
case "$out" in
  *"kb memory is available"*) ok "first prompt emits the memory protocol" ;;
  *) bad "first prompt emits the memory protocol (got: $out)" ;;
esac
case "$out" in
  *"Recent memories:"*"- T1  [main]"*) ok "first prompt emits the recent-memories index" ;;
  *) bad "first prompt emits the recent-memories index (got: $out)" ;;
esac
case "$out" in
  *"Pending distill (kimi): 1 session(s)"*"$SID"*) ok "kimi ledger entry surfaced with harness label" ;;
  *) bad "kimi ledger entry surfaced with harness label (got: $out)" ;;
esac
if [ ! -s "$CACHE1/kb/distill-pending" ]; then
  ok "ledger consumed after surfacing"
else
  bad "ledger consumed after surfacing (ledger: $(cat "$CACHE1/kb/distill-pending"))"
fi
if [ -f "$CACHE1/kb/$MARKER_NAME" ]; then
  ok "once-per-session marker created"
else
  bad "once-per-session marker created (dir: $(ls "$CACHE1/kb" 2>/dev/null))"
fi

# --- 2. second prompt: marker gate stays silent -------------------------
out2="$(run_wake "$CACHE1")"
if [ -z "$out2" ]; then
  ok "second prompt is silent (marker gate)"
else
  bad "second prompt is silent (got: $out2)"
fi

# --- 3. no ledger: protocol+index still emitted once, then silent -------
CACHE3="$TMPROOT/cache3"
out3="$(run_wake "$CACHE3")"
case "$out3" in
  *"kb memory is available"*"Recent memories:"*) ok "no ledger -> protocol+index still emitted" ;;
  *) bad "no ledger -> protocol+index still emitted (got: $out3)" ;;
esac
case "$out3" in
  *"Pending distill"*) bad "no ledger -> no pending block" ;;
  *) ok "no ledger -> no pending block" ;;
esac
out3b="$(run_wake "$CACHE3")"
if [ -z "$out3b" ]; then
  ok "no-ledger second prompt is silent"
else
  bad "no-ledger second prompt is silent (got: $out3b)"
fi

# --- 4. mixed-harness ledger: label lists both, comma-joined ------------
CACHE4="$TMPROOT/cache4"
mkdir -p "$CACHE4/kb"
now="$(date +%s)"
{
  printf 'kimi k-sid %s\n' "$now"
  printf 'grok g-sid %s\n' "$now"
} >"$CACHE4/kb/distill-pending"
out4="$(run_wake "$CACHE4")"
case "$out4" in
  *"Pending distill (grok,kimi): 2 session(s)"*"g-sid"*"k-sid"*) ok "mixed ledger labeled grok,kimi" ;;
  *) bad "mixed ledger labeled grok,kimi (got: $out4)" ;;
esac

# --- 5. no session_id in payload -> silent, no marker --------------------
CACHE5="$TMPROOT/cache5"
out5="$(export XDG_CACHE_HOME="$CACHE5"; mkdir -p "$CACHE5"; printf '%s' '{"cwd":"/tmp"}' | "$WAKE")"
if [ -z "$out5" ] && [ ! -d "$CACHE5/kb" ]; then
  ok "missing session_id -> silent, no marker"
else
  bad "missing session_id -> silent, no marker (got: $out5)"
fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
