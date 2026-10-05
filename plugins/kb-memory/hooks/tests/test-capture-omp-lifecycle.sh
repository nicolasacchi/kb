#!/usr/bin/env bash
# test-capture-omp-lifecycle.sh - the LIFECYCLE of kb-capture-omp.sh (v0.45 OC):
# per-session exclusion + coalescing, freshness, cancellation that reaps every
# owned descendant, recovery, the fingerprint shortcut, and the translator's
# linear leaf-chain walk. Real subprocesses throughout: a `jq` shim (logs the
# start/end of every TRANSLATE run and can hold it open) stands in front of the
# real jq, and a fake `kb` (tests/fixtures/fake-capture-kb.sh) lands the HTML.
#
# Every process this test starts is recorded; cleanup kills only those (never a
# name-based kill) and the run ends by asserting nothing of ours is left.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-capture-omp-lifecycle.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CAPTURE="$HOOKS_DIR/kb-capture-omp.sh"
FIX="$SCRIPT_DIR/fixtures"
export HOOKS_DIR

for need in jq flock setsid ps timeout; do
  if ! command -v "$need" >/dev/null 2>&1; then
    # CI (GitHub sets CI=true) has all of them: a missing tool there must fail,
    # not skip, or the lane would silently pin nothing.
    if [ -n "${CI:-}" ]; then echo "not ok  - $need is required in CI"; exit 1; fi
    echo "SKIP: $need is required"; echo "passed=0 failed=0 skipped=1"; exit 0
  fi
done

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-omp-lifecycle.XXXXXX")"
STARTED_PIDS=()
cleanup() {
  local p
  for p in "${STARTED_PIDS[@]+"${STARTED_PIDS[@]}"}"; do
    # Our own captures are session leaders: reap their session, then the pid.
    for q in $(ps -s "$p" -o pid= 2>/dev/null); do kill -KILL "$q" 2>/dev/null; done
    kill -KILL "$p" 2>/dev/null
  done
  rm -rf "$TMPROOT"
}
trap cleanup EXIT

REAL_JQ="$(command -v jq)"
export REAL_JQ
mkdir -p "$TMPROOT/shim" "$TMPROOT/kbbin" "$TMPROOT/tmp" "$TMPROOT/home"
export TMPDIR="$TMPROOT/tmp" HOME="$TMPROOT/home"
export XDG_CACHE_HOME="$TMPROOT/home/.cache"
unset KB_CACHE_DIR KB_STATE_DIR KB_CONFIG_DIR
export KB_CAPTURE_LOCKS="$TMPROOT/locks"
export KB_CAPTURE_SPOOL="$TMPROOT/spool"
export KB_SESSIONS_DIR="$TMPROOT/sessions"
mkdir -p "$KB_SESSIONS_DIR"

# jq shim: every TRANSLATE run (the only invocation carrying `--arg file`) is
# logged "start|end <pid> <source-file>" and held open for $SHIM_SLEEP seconds.
cat >"$TMPROOT/shim/jq" <<'SHIM'
#!/usr/bin/env bash
file=""
args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
  if [ "${args[i]}" = "--arg" ] && [ "${args[i + 1]:-}" = "file" ]; then file="${args[i + 2]:-}"; fi
done
case " $* " in
  *" --arg file "*)
    echo "start $$ $file" >>"$SHIM_LOG"
    [ -n "${SHIM_SLEEP:-}" ] && sleep "$SHIM_SLEEP"
    "$REAL_JQ" "$@"
    rc=$?
    echo "end $$ $file" >>"$SHIM_LOG"
    exit "$rc"
    ;;
  *) exec "$REAL_JQ" "$@" ;;
esac
SHIM
chmod +x "$TMPROOT/shim/jq"

# fake kb: lands through the fixture stand-in unless the fail flag exists.
cat >"$TMPROOT/kbbin/kb" <<'KB'
#!/usr/bin/env bash
if [ -e "$KB_FAIL_FLAG" ] && [ "${1:-}" = "sessions" ] && [ "${2:-}" = "capture" ]; then exit 1; fi
if [ -n "${SIDECAR_DUMP:-}" ] && [ "${1:-}" = "sessions" ] && [ "${2:-}" = "capture" ]; then
  prev=""
  for a in "$@"; do
    [ "$prev" = "--transcript" ] && ls "$(dirname "$a")"/*/subagents 2>/dev/null >>"$SIDECAR_DUMP"
    prev="$a"
  done
fi
exec bash "$FAKE_CAPTURE_KB" "$@"
KB
chmod +x "$TMPROOT/kbbin/kb"
export FAKE_CAPTURE_KB="$FIX/fake-capture-kb.sh"
export KB_FAIL_FLAG="$TMPROOT/kb-fail"
export SHIM_LOG="$TMPROOT/shim.log"
export PATH="$TMPROOT/shim:$TMPROOT/kbbin:$PATH"
: >"$SHIM_LOG"

# --- helpers -----------------------------------------------------------------
# mk_session <path> <session-id> <title>: the real on-disk shape - a fixed
# 256-byte title slot, then the fixture body with its session id swapped.
mk_session() {
  local path="$1" sid="$2" title="$3" slot
  slot="$(printf '{"type":"title","v":1,"title":"%s"}' "$title")"
  {
    printf '%s%*s\n' "$slot" $((256 - ${#slot} - 1)) ''
    sed "s/fx01a034-0000-0000-0000-000000000001/$sid/g" "$FIX/omp-session-commit.jsonl"
  } >"$path"
}
append_marker() { # <path> <marker>
  local last
  last="$("$REAL_JQ" -r 'select(.id != null) | .id' "$1" | tail -1)"
  printf '{"type":"message","id":"zz-%s","parentId":"%s","timestamp":"2026-08-24T10:09:00.000Z","message":{"role":"user","content":[{"type":"text","text":"%s"}]}}\n' \
    "$2" "$last" "$2" >>"$1"
}
hook_input() { printf '{"session_file":"%s","session_id":"%s","cwd":"%s"}' "$1" "$2" "$TMPROOT"; }
# hook_bg <session-file> <sid>: start a hook-mode capture exactly the way the
# pre-OC kb-omp.ts did (plain non-detached spawn); records its pid.
HOOK_N=0
hook_bg() {
  HOOK_N=$((HOOK_N + 1))
  hook_input "$1" "$2" >"$TMPROOT/in.$HOOK_N"
  bash "$CAPTURE" <"$TMPROOT/in.$HOOK_N" >/dev/null 2>&1 &
  LAST_PID=$!
  STARTED_PIDS+=("$LAST_PID")
}
hook_fg() { hook_input "$1" "$2" | bash "$CAPTURE" >/dev/null 2>&1; }
starts() { grep -c "^start .* $1\$" "$SHIM_LOG" 2>/dev/null || true; }
wait_for() { # <seconds> <cmd...> - poll until cmd succeeds
  local n=$(($1 * 20)) i
  shift
  for ((i = 0; i < n; i++)); do "$@" && return 0; sleep 0.05; done
  return 1
}
log_has_start() { grep -q "^start .* $1\$" "$SHIM_LOG"; }
html_of() { ls "$KB_SESSIONS_DIR"/session-*-"$1".html 2>/dev/null | head -1; }
max_concurrency() { # over the shim log
  awk '/^start/{c++; if(c>m)m=c} /^end/{c--} END{print m+0}' "$SHIM_LOG"
}
lock_free() { # <session-file>: its lock can be taken right now
  local f
  for f in "$KB_CAPTURE_LOCKS"/*.lock; do
    [ -e "$f" ] || continue
    ( flock -n 8 ) 8>"$f" || return 1
  done
  return 0
}
no_scratch() { ! ls -d "$TMPDIR"/tmp.* >/dev/null 2>&1; }

echo "== kb-capture-omp.sh lifecycle (v0.45 OC) =="

# ---------------------------------------------------------------------------
# 1. Overlapping captures of ONE session: one conversion at a time, the newest
#    state is published, nothing coalesced is dropped, and a CLI backfill waits.
S1="$TMPROOT/s1.jsonl"; SID1="a1000000-0000-0000-0000-000000000001"
mk_session "$S1" "$SID1" "one"
: >"$SHIM_LOG"
export SHIM_SLEEP=3
hook_bg "$S1" "$SID1"; A=$LAST_PID
if wait_for 10 log_has_start "$S1"; then
  append_marker "$S1" "APPENDED-MARKER-ONE"
  t0=$SECONDS
  hook_bg "$S1" "$SID1"; B=$LAST_PID
  hook_bg "$S1" "$SID1"; C=$LAST_PID
  hook_bg "$S1" "$SID1"; D=$LAST_PID
  wait "$B" "$C" "$D"
  if kill -0 "$A" 2>/dev/null; then
    ok "overlapping requests return at once while the owner is still converting"
  else
    bad "the owner had already finished - the overlap was not exercised"
  fi
  # a CLI backfill of the same file blocks on the lock instead of overlapping
  bash "$CAPTURE" "$S1" >/dev/null 2>&1 &
  CLI=$!; STARTED_PIDS+=("$CLI")
  wait "$A" "$CLI"
else
  bad "the first conversion never started"
fi
unset SHIM_SLEEP
if [ "$(max_concurrency)" = "1" ]; then
  ok "never more than one conversion alive for one session"
else
  bad "conversions overlapped (max concurrency $(max_concurrency))"
fi
h="$(html_of "$SID1")"
if [ -n "$h" ] && grep -q 'APPENDED-MARKER-ONE' "$h"; then
  ok "the newest source state was published (an append during conversion is not lost)"
else
  bad "the published capture is stale (no APPENDED-MARKER-ONE)"
fi
if [ "$(starts "$S1")" -ge 2 ]; then
  ok "the request that arrived mid-conversion was coalesced into a fresh pass, not dropped"
else
  bad "no second pass ran (starts=$(starts "$S1"))"
fi
if no_scratch && lock_free; then
  ok "scratch removed and lock released after the overlap run"
else
  bad "leftover scratch or a held lock after the overlap run"
fi

# ---------------------------------------------------------------------------
# 2. Independent sessions never contend.
S2="$TMPROOT/s2.jsonl"; SID2="a2000000-0000-0000-0000-000000000002"
S3="$TMPROOT/s3.jsonl"; SID3="a3000000-0000-0000-0000-000000000003"
mk_session "$S2" "$SID2" "two"; mk_session "$S3" "$SID3" "three"
: >"$SHIM_LOG"
export SHIM_SLEEP=1.2
hook_bg "$S2" "$SID2"; P2=$LAST_PID
hook_bg "$S3" "$SID3"; P3=$LAST_PID
wait "$P2" "$P3"
unset SHIM_SLEEP
if [ "$(max_concurrency)" = "2" ]; then
  ok "two different sessions convert concurrently"
else
  bad "independent sessions were serialised (max concurrency $(max_concurrency))"
fi
if [ -n "$(html_of "$SID2")" ] && [ -n "$(html_of "$SID3")" ]; then
  ok "both independent sessions were captured"
else
  bad "an independent session was not captured"
fi

# ---------------------------------------------------------------------------
# 3. SIGTERM mid-conversion (what the pre-OC kb-omp.ts sent: TERM to the shell
#    pid only): every owned descendant dies, lock released, scratch removed,
#    the previous capture is untouched, and the fingerprint is NOT advanced.
S4="$TMPROOT/s4.jsonl"; SID4="a4000000-0000-0000-0000-000000000004"
mk_session "$S4" "$SID4" "four"
hook_fg "$S4" "$SID4"
h4="$(html_of "$SID4")"
before_sum="$(sha256sum "$h4" | cut -d' ' -f1)"
done_before="$(cat "$KB_CAPTURE_LOCKS"/*.done 2>/dev/null | sha256sum)"
append_marker "$S4" "NEVER-LANDS"
: >"$SHIM_LOG"
export SHIM_SLEEP=60
hook_bg "$S4" "$SID4"; K=$LAST_PID
if wait_for 10 log_has_start "$S4"; then
  sleep 0.3
  members="$(ps -s "$K" -o pid= | tr -d ' ' | grep -vx "$K" | tr '\n' ' ')"
  shim_pids="$(awk -v f="$S4" '$1=="start" && $3==f {print $2}' "$SHIM_LOG")"
  STARTED_PIDS+=($shim_pids)
  if [ -n "$(echo "$members" | tr -d ' ')" ]; then
    ok "the capture runs as a session leader with owned children ($(echo "$members" | wc -w) processes)"
  else
    bad "no owned children were found to terminate"
  fi
  kill -TERM "$K"
  gone=0
  for _ in $(seq 1 100); do kill -0 "$K" 2>/dev/null || { gone=1; break; }; sleep 0.05; done
  wait "$K" 2>/dev/null
  [ "$gone" = 1 ] && ok "the capture exited promptly on SIGTERM" || bad "the capture ignored SIGTERM"
  left=""
  for p in $members $shim_pids; do kill -0 "$p" 2>/dev/null && left="$left $p"; done
  left="$left $(ps -s "$K" -o pid= | tr -d ' ' | tr '\n' ' ')"
  if [ -z "$(echo "$left" | tr -d ' ')" ]; then
    ok "no owned descendant (jq shim, sleep, ...) survives the SIGTERM"
  else
    bad "descendants survived:$left"
  fi
else
  bad "the long conversion never started"
fi
unset SHIM_SLEEP
if no_scratch; then ok "scratch files removed on SIGTERM"; else bad "scratch leaked on SIGTERM: $(ls "$TMPDIR")"; fi
if lock_free; then ok "lock released on SIGTERM"; else bad "lock still held after SIGTERM"; fi
if [ "$(sha256sum "$h4" | cut -d' ' -f1)" = "$before_sum" ] && ! grep -q NEVER-LANDS "$h4"; then
  ok "the previous valid capture is byte-identical after the interrupted run"
else
  bad "the previous capture changed"
fi
if [ "$(cat "$KB_CAPTURE_LOCKS"/*.done 2>/dev/null | sha256sum)" = "$done_before" ]; then
  ok "an interrupted run records nothing as captured"
else
  bad "the fingerprint advanced although nothing landed"
fi

# 4. Recovery: the very next capture succeeds and publishes the new state.
hook_fg "$S4" "$SID4"
if grep -q NEVER-LANDS "$(html_of "$SID4")" && lock_free && no_scratch; then
  ok "recovery: the next capture after an interrupted one lands the newest state"
else
  bad "the capture after an interruption did not land"
fi

# ---------------------------------------------------------------------------
# 5. The hard deadline makes the caller's timeout real: a conversion that never
#    ends is stopped, its children reaped, nothing recorded, previous capture kept.
append_marker "$S4" "DEADLINE-MARKER"
before_sum="$(sha256sum "$(html_of "$SID4")" | cut -d' ' -f1)"
: >"$SHIM_LOG"
export SHIM_SLEEP=60
hook_input "$S4" "$SID4" >"$TMPROOT/in.dl"
t0=$SECONDS
KB_CAPTURE_HARD_SECS=2 timeout -k 5 40 bash "$CAPTURE" <"$TMPROOT/in.dl" >/dev/null 2>&1 &
DL=$!; STARTED_PIDS+=("$DL")
wait "$DL"; elapsed=$((SECONDS - t0))
unset SHIM_SLEEP
leftover="$(ps -s "$DL" -o pid= | tr -d ' ' | tr '\n' ' ')"
if [ "$elapsed" -le 15 ] && [ -z "$(echo "$leftover" | tr -d ' ')" ]; then
  ok "KB_CAPTURE_HARD_SECS bounds a stuck conversion (${elapsed}s) and leaves no process behind"
else
  bad "deadline not enforced: ${elapsed}s, leftover:$leftover"
fi
if [ "$(sha256sum "$(html_of "$SID4")" | cut -d' ' -f1)" = "$before_sum" ] && lock_free && no_scratch; then
  ok "deadline expiry keeps the previous capture, frees the lock and removes scratch"
else
  bad "state after a deadline expiry is wrong"
fi

# ---------------------------------------------------------------------------
# 6. Fingerprint shortcut: skips only when nothing changed.
S5="$TMPROOT/s5.jsonl"; SID5="a5000000-0000-0000-0000-000000000005"
mk_session "$S5" "$SID5" "five"
: >"$SHIM_LOG"
hook_fg "$S5" "$SID5"; hook_fg "$S5" "$SID5"
if [ "$(starts "$S5")" = "1" ]; then
  ok "an unchanged session is not converted twice"
else
  bad "unchanged session converted $(starts "$S5") times"
fi
# in-place title rewrite at identical length (omp rewrites the slot in place)
slot="$(printf '{"type":"title","v":1,"title":"%s"}' "five-renamed")"
size_before="$(stat -c %s "$S5")"
printf '%s%*s\n' "$slot" $((256 - ${#slot} - 1)) '' | dd of="$S5" conv=notrunc status=none
sleep 0.05
hook_fg "$S5" "$SID5"
if [ "$(stat -c %s "$S5")" = "$size_before" ] && [ "$(starts "$S5")" = "2" ] \
  && grep -q 'five-renamed' "$(html_of "$SID5")"; then
  ok "an in-place title rewrite (same size) is recaptured"
else
  bad "in-place retitle missed (starts=$(starts "$S5"))"
fi
# subagent sidecar appears next to the session
mkdir -p "${S5%.jsonl}"
cp "$FIX/omp-subagent.jsonl" "${S5%.jsonl}/Explorer.jsonl"
n_before="$(starts "$S5")"
hook_fg "$S5" "$SID5"
if [ "$(starts "$S5")" -gt "$n_before" ]; then
  ok "a new subagent sidecar triggers a recapture"
else
  bad "sidecar add did not recapture"
fi
n_before="$(starts "$S5")"
hook_fg "$S5" "$SID5"
[ "$(starts "$S5")" = "$n_before" ] && ok "...and the following unchanged run is skipped again" || bad "unchanged run after sidecar recaptured"
# sidecar content change with the parent untouched
printf '{"type":"message","id":"sx9","parentId":"s3","timestamp":"2026-08-24T11:06:00.000Z","message":{"role":"user","content":[{"type":"text","text":"more"}]}}\n' >>"${S5%.jsonl}/Explorer.jsonl"
hook_fg "$S5" "$SID5"
[ "$(starts "$S5")" -gt "$n_before" ] && ok "a sidecar-only change recaptures" || bad "sidecar-only change missed"
# the capture file vanished: never skip
rm -f "$(html_of "$SID5")"
n_before="$(starts "$S5")"
hook_fg "$S5" "$SID5"
if [ "$(starts "$S5")" -gt "$n_before" ] && [ -n "$(html_of "$SID5")" ]; then
  ok "a deleted capture is recreated even though the input is unchanged"
else
  bad "deleted capture was not recreated"
fi
# failed landing is NOT recorded as captured
append_marker "$S5" "AFTER-FAIL"
: >"$KB_FAIL_FLAG"
hook_fg "$S5" "$SID5"
rm -f "$KB_FAIL_FLAG"
n_before="$(starts "$S5")"
hook_fg "$S5" "$SID5"
if [ "$(starts "$S5")" -gt "$n_before" ] && grep -q AFTER-FAIL "$(html_of "$SID5")"; then
  ok "a failed (spooled) landing is not recorded as captured - the retry converts and lands"
else
  bad "a failed landing was treated as done"
fi

# ---------------------------------------------------------------------------
# 6b. SIGKILL of the capture itself (an external kill, the OOM killer, the
#     caller's group-kill backstop): no trap runs, and timeout(1) regroups its
#     child, so a group kill misses jq. The lock must still die with the owner,
#     the watchdog must reap the survivors + remove the scratch, and a request
#     that arrives meanwhile must NOT be lost.
kill_case() { # <label> <owner|group>
  local label="$1" how="$2" S sid K members shim_pids q left
  S="$TMPROOT/k-$label.jsonl"; sid="$(printf 'b%s0000-0000-0000-0000-000000000009' "$label" | cut -c1-36)"
  mk_session "$S" "$sid" "kill-$label"
  : >"$SHIM_LOG"
  export SHIM_SLEEP=60
  hook_bg "$S" "$sid"; K=$LAST_PID
  if ! wait_for 10 log_has_start "$S"; then bad "[$label] the long conversion never started"; unset SHIM_SLEEP; return; fi
  sleep 0.3
  members="$(ps -s "$K" -o pid= | tr -d ' ' | grep -vx "$K" | tr '\n' ' ')"
  shim_pids="$(awk -v f="$S" '$1=="start" && $3==f {print $2}' "$SHIM_LOG")"
  STARTED_PIDS+=($members $shim_pids)
  if [ "$how" = group ]; then kill -KILL -- "-$K" 2>/dev/null; else kill -KILL "$K" 2>/dev/null; fi
  wait "$K" 2>/dev/null
  unset SHIM_SLEEP
  # (1) the lock is free at once although the orphaned jq may still run
  if wait_for 3 lock_free; then ok "[$label] SIGKILL: the lock is released with the owner (children do not hold it)"; else bad "[$label] SIGKILL: the lock is still held"; fi
  # (2) a request arriving in that window is served, not dropped
  append_marker "$S" "KILL-WINDOW-$label"
  hook_fg "$S" "$sid"
  if grep -q "KILL-WINDOW-$label" "$(html_of "$sid")" 2>/dev/null; then
    ok "[$label] SIGKILL: the next request publishes the newest state (not lost)"
  else
    bad "[$label] SIGKILL: the request after the kill was lost"
  fi
  # (3) the watchdog reaps every survivor and removes the scratch
  for _ in $(seq 1 100); do
    left=""
    for q in $members $shim_pids; do kill -0 "$q" 2>/dev/null && left="$left $q"; done
    [ -z "$left" ] && no_scratch && break
    sleep 0.1
  done
  if [ -z "$left" ]; then ok "[$label] SIGKILL: no orphaned jq/timeout/sleep survives"; else bad "[$label] SIGKILL: orphans survived:$left"; fi
  if no_scratch; then ok "[$label] SIGKILL: scratch removed"; else bad "[$label] SIGKILL: scratch leaked: $(ls "$TMPDIR")"; fi
}
kill_case owner owner
kill_case group group

# 6c. One poisoned subagent sidecar must not take the whole capture down: the
#     main transcript and the healthy sidecars still land.
S6="$TMPROOT/s6.jsonl"; SID6="a6000000-0000-0000-0000-000000000006"
mk_session "$S6" "$SID6" "six"
mkdir -p "${S6%.jsonl}"
cp "$FIX/omp-subagent.jsonl" "${S6%.jsonl}/Alpha.jsonl"
n=0
for poison in '{"type":"message","id":"p1","parentId":null,"message":"str"}' '[1,2]' '"justastring"' \
  '{"type":"message","id":"p1","parentId":null,"message":{"role":"assistant","content":[1,"a"]}}' \
  '{"type":"compaction","id":"p1","parentId":null,"summary":5}'; do
  n=$((n + 1))
  printf '%s\n' "$poison" >"${S6%.jsonl}/Poison.jsonl"
  rm -f "$(html_of "$SID6")"; : >"$TMPROOT/sidecars.txt"
  append_marker "$S6" "POISON-$n"
  SIDECAR_DUMP="$TMPROOT/sidecars.txt" hook_fg "$S6" "$SID6"
  if grep -q "POISON-$n" "$(html_of "$SID6")" 2>/dev/null && grep -q 'agent-Alpha.jsonl' "$TMPROOT/sidecars.txt" \
    && ! grep -q 'agent-Poison.jsonl' "$TMPROOT/sidecars.txt"; then
    ok "poison sidecar #$n is dropped; the main transcript and the healthy sidecar still land"
  else
    bad "poison sidecar #$n broke the capture (html: $(html_of "$SID6"), sidecars: $(tr '\n' ' ' <"$TMPROOT/sidecars.txt"))"
  fi
done

# ---------------------------------------------------------------------------
# 7. Translator: the linear leaf-chain walk is byte-identical to the former
#    quadratic reduce and does not blow up with chain length.
eval "$(sed -n "/^TRANSLATE='/,/^'\$/p" "$CAPTURE")"
NEWJQ="$TMPROOT/new.jq"; OLDJQ="$TMPROOT/old.jq"
printf '%s\n' "$TRANSLATE" >"$NEWJQ"
awk '
  /# Linear walk \(v0\.45 OC\)/ { skip = 1;
    print "  (reduce range(($n - 1); -1; -1) as $i (";
    print "     {cur: ($es[$n - 1].id // null), keep: []};";
    print "     if .cur != null and ($es[$i].id // null) == .cur";
    print "     then {cur: ($es[$i].parentId // null), keep: ([$es[$i]] + .keep)}";
    print "     else . end)).keep as $chain |" }
  skip && /as \$chain \|/ { skip = 0; next }
  !skip { print }' "$NEWJQ" >"$OLDJQ"
translate_with() { # <prog> <file>
  "$REAL_JQ" -R -c 'fromjson? // empty' "$2" | "$REAL_JQ" -c -s --arg file "$2" -f "$1"
}
same=1
for f in "$FIX"/omp-*.jsonl; do
  a="$(translate_with "$OLDJQ" "$f" | sha256sum)"
  b="$(translate_with "$NEWJQ" "$f" | sha256sum)"
  [ "$a" = "$b" ] || { same=0; echo "  differs: $f"; }
done
if command -v python3 >/dev/null 2>&1; then
  python3 "$FIX/gen-omp-chain.py" 3000 "$TMPROOT/chain3k.jsonl" >/dev/null 2>&1
  a="$(translate_with "$OLDJQ" "$TMPROOT/chain3k.jsonl" | sha256sum)"
  b="$(translate_with "$NEWJQ" "$TMPROOT/chain3k.jsonl" | sha256sum)"
  [ "$a" = "$b" ] || { same=0; echo "  differs: synthetic 3000-record chain"; }
fi
if [ "$same" = 1 ] && [ -n "$(translate_with "$NEWJQ" "$FIX/omp-session-ok4.jsonl")" ]; then
  ok "translator output is byte-identical to the former reduce (fixtures, edge cases, linked 3k chain)"
else
  bad "translator output changed"
fi
if command -v python3 >/dev/null 2>&1; then
  python3 "$FIX/gen-omp-chain.py" 14000 "$TMPROOT/chain14k.jsonl" >/dev/null 2>&1
  "$REAL_JQ" -R -c 'fromjson? // empty' "$TMPROOT/chain14k.jsonl" >"$TMPROOT/chain14k.clean"
  t0=$(date +%s%N)
  timeout -k 5 120 "$REAL_JQ" -c -s --arg file x -f "$NEWJQ" "$TMPROOT/chain14k.clean" >"$TMPROOT/n.out"
  t_new=$((($(date +%s%N) - t0) / 1000000))
  t0=$(date +%s%N)
  timeout -k 5 240 "$REAL_JQ" -c -s --arg file x -f "$OLDJQ" "$TMPROOT/chain14k.clean" >"$TMPROOT/o.out"
  t_old=$((($(date +%s%N) - t0) / 1000000))
  echo "        14k-record chain: new ${t_new} ms, former reduce ${t_old} ms"
  if cmp -s "$TMPROOT/n.out" "$TMPROOT/o.out" && [ "$((t_new * 2))" -lt "$t_old" ]; then
    ok "the leaf-chain walk is linear: at least 2x faster than the former reduce on a 14k-record chain"
  else
    bad "no speed-up on a long chain (new ${t_new} ms vs old ${t_old} ms) or outputs differ"
  fi
fi

# ---------------------------------------------------------------------------
# Nothing of ours is left running.
left=""
for p in "${STARTED_PIDS[@]+"${STARTED_PIDS[@]}"}"; do
  kill -0 "$p" 2>/dev/null && left="$left $p"
  left="$left $(ps -s "$p" -o pid= 2>/dev/null | tr -d ' ' | tr '\n' ' ')"
done
if [ -z "$(echo "$left" | tr -d ' ')" ]; then
  ok "no process started by this test remains"
else
  bad "processes remain:$left"
fi

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
