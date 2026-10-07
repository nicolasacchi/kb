#!/usr/bin/env bash
# test-capture-omp-segments.sh - SEGMENTED omp capture (v0.46 SEG-PR2): a long
# session is landed as an ordered chain of ordinary sessions (part 1 = the bare
# id, part k>=2 = <id>-p<NN>) planned by `kb sessions segment-plan`.
#
# Real subprocesses throughout. `kb` is a dispatcher: capture lands through the
# fixture stand-in (fake-capture-kb.sh); the planner and `drop-part` are the
# REAL ones when the kb on PATH / in KB_BIN_DIR has them (CI builds kb from
# source) and otherwise the offline stand-ins (fake-segment-plan.py + a bash
# drop-part), so the test also runs on a host with an older kb. The first line
# of output says which.
#
# Every process this test starts is recorded; cleanup kills only those (never a
# name-based kill) and the run ends by asserting nothing of ours is left.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-capture-omp-segments.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CAPTURE="$HOOKS_DIR/kb-capture-omp.sh"
FIX="$SCRIPT_DIR/fixtures"
export HOOKS_DIR

for need in jq flock setsid ps timeout python3 awk; do
  if ! command -v "$need" >/dev/null 2>&1; then
    if [ -n "${CI:-}" ]; then echo "not ok  - $need is required in CI"; exit 1; fi
    echo "SKIP: $need is required"; echo "passed=0 failed=0 skipped=1"; exit 0
  fi
done

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-omp-seg.XXXXXX")"
STARTED_PIDS=()
cleanup() {
  local p q
  for p in "${STARTED_PIDS[@]+"${STARTED_PIDS[@]}"}"; do
    for q in $(ps -s "$p" -o pid= 2>/dev/null); do kill -KILL "$q" 2>/dev/null; done
    kill -KILL "$p" 2>/dev/null
  done
  rm -rf "${TMPROOT:?}"
}
trap cleanup EXIT

# The kb the dispatcher may delegate to (resolved BEFORE PATH is rewritten).
REAL_KB="$(command -v kb 2>/dev/null || true)"
[ -n "${KB_BIN_DIR:-}" ] && [ -x "$KB_BIN_DIR/kb" ] && REAL_KB="$KB_BIN_DIR/kb"
REAL_PLANNER=""
REAL_DROP=""
if [ -n "$REAL_KB" ]; then
  "$REAL_KB" sessions segment-plan --help >/dev/null 2>&1 && REAL_PLANNER=1
  "$REAL_KB" sessions drop-part --help >/dev/null 2>&1 && REAL_DROP=1
fi
export REAL_KB REAL_PLANNER REAL_DROP
if [ -n "$REAL_PLANNER" ] && [ -n "$REAL_DROP" ]; then
  echo "== segmented omp capture (v0.46 SEG-PR2): REAL planner + REAL drop-part =="
else
  echo "== segmented omp capture (v0.46 SEG-PR2): offline stand-ins (planner=${REAL_PLANNER:-fake} drop-part=${REAL_DROP:-fake}) =="
fi

# CI builds kb from source: there the REAL planner and drop-part are mandatory,
# or the lane would silently pin only the offline stand-ins.
if [ -n "${CI:-}" ] && { [ -z "$REAL_PLANNER" ] || [ -z "$REAL_DROP" ]; }; then
  bad "CI must run against the real planner and drop-part (planner=${REAL_PLANNER:-missing} drop-part=${REAL_DROP:-missing}; KB_BIN_DIR=${KB_BIN_DIR:-unset})"
fi

mkdir -p "$TMPROOT/kbbin" "$TMPROOT/tmp" "$TMPROOT/home"
export TMPDIR="$TMPROOT/tmp" HOME="$TMPROOT/home"
export XDG_CACHE_HOME="$TMPROOT/home/.cache"
export XDG_CONFIG_HOME="$TMPROOT/home/.config"
unset KB_CACHE_DIR KB_STATE_DIR KB_CONFIG_DIR KB_CAPTURE_SEGMENTS
export KB_CAPTURE_LOCKS="$TMPROOT/locks"
export KB_CAPTURE_SPOOL="$TMPROOT/spool"
export KB_SESSIONS_DIR="$TMPROOT/sessions"
export KB_CAPTURE_TRACE="$TMPROOT/trace"
export KB_LOG="$TMPROOT/kb.log"
export KB_FAIL_FLAG="$TMPROOT/kb-fail"
export KB_DROP_FAIL="$TMPROOT/kb-drop-fail"
export KB_HANGING="$TMPROOT/kb-hanging"
export KB_CALLS="$TMPROOT/kb-calls"
export FAKE_CAPTURE_KB="$FIX/fake-capture-kb.sh"
export FAKE_PLAN="$FIX/fake-segment-plan.py"
export KB_CAPTURE_SEGMENT_BYTES=6000
export KB_CAPTURE_BUDGET_SECS=25

cat >"$TMPROOT/kbbin/kb" <<'KB'
#!/usr/bin/env bash
# dispatcher: see the header of test-capture-omp-segments.sh
if [ "${1:-}" = "sessions" ]; then
  case "${2:-}" in
    segment-plan)
      if [ -n "${KB_NO_PLANNER:-}" ]; then echo "error: unrecognized subcommand 'segment-plan'" >&2; exit 2; fi
      case " $* " in
        *" --print-chain "*) [ -n "${KB_CHAIN_SLEEP:-}" ] && { echo "chain $$" >>"$KB_LOG"; sleep "$KB_CHAIN_SLEEP"; } ;;
      esac
      case " $* " in
        *" --emit "*) [ -n "${KB_EMIT_SLEEP:-}" ] && { echo "emit $$" >>"$KB_LOG"; sleep "$KB_EMIT_SLEEP"; } ;;
      esac
      if [ -n "$REAL_PLANNER" ]; then exec "$REAL_KB" "$@"; fi
      shift 2
      exec python3 "$FAKE_PLAN" "$@"
      ;;
    drop-part)
      case " $* " in *" --help "*) ;; *) [ -e "$KB_DROP_FAIL" ] && { echo "drop-fail $*" >>"$KB_LOG"; exit 1; } ;; esac
      if [ -n "$REAL_DROP" ]; then exec "$REAL_KB" "$@"; fi
      case " $* " in *" --help "*) exit 0 ;; esac
      shift 2
      pid="" of="" out=""
      while [ "$#" -gt 0 ]; do
        case "$1" in
          --session-id) pid="$2"; shift 2 ;;
          --segment-of) of="$2"; shift 2 ;;
          --out) out="$2"; shift 2 ;;
          *) shift ;;
        esac
      done
      . "$HOOKS_DIR/kb-hook-lib.sh"
      case "$pid" in "$of"-p[0-9]*) ;; *) exit 2 ;; esac
      key="$(hook_sid_key "$pid")"
      for f in "$out"/session-????????T??????Z-"$key.html"; do
        [ -f "$f" ] || continue
        if grep -q "\"segmentOf\":\"$of\"" "$f" && grep -q "\"sessionId\":\"$pid\"" "$f"; then rm -f "$f"; else exit 3; fi
      done
      exit 0
      ;;
    capture)
      if [ -e "$KB_FAIL_FLAG" ]; then exit 1; fi
      sid="" t=""
      prev=""
      for a in "$@"; do
        [ "$prev" = "--session-id" ] && sid="$a"
        [ "$prev" = "--transcript" ] && t="$a"
        prev="$a"
      done
      if [ -n "${KB_FAIL_SID_SUFFIX:-}" ]; then case "$sid" in *"$KB_FAIL_SID_SUFFIX") exit 1 ;; esac; fi
      if [ -n "${KB_REAL_CAPTURE:-}" ]; then exec "$REAL_KB" "$@"; fi
      if [ -n "$t" ]; then
        n=$(($(cat "$KB_CALLS" 2>/dev/null || echo 0) + 1)); echo "$n" >"$KB_CALLS"
        sc="$(ls "$(dirname "$t")"/*/subagents/*.jsonl 2>/dev/null | xargs -r -n1 basename | tr '\n' ',')"
        echo "capture $sid bytes=$(stat -c %s "$t") sidecars=$sc" >>"$KB_LOG"
        if [ -n "${KB_CAP_SLEEP_OVER:-}" ]; then
          sb="$(cat "$(dirname "$t")"/*/subagents/*.jsonl 2>/dev/null | wc -c)"
          [ "$sb" -gt "$KB_CAP_SLEEP_OVER" ] && sleep "${KB_CAP_SLEEP:-1}"
        fi
        if [ -n "${KB_HANG_AT:-}" ] && [ "$n" = "$KB_HANG_AT" ]; then
          echo "$$" >"$KB_HANGING"
          sleep 300
        fi
      fi
      exec bash "$FAKE_CAPTURE_KB" "$@"
      ;;
  esac
fi
exec bash "$FAKE_CAPTURE_KB" "$@"
KB
chmod +x "$TMPROOT/kbbin/kb"
export PATH="$TMPROOT/kbbin:$PATH"

# --- helpers -----------------------------------------------------------------
GEN="$FIX/gen-omp-turns.py"
SEQ=0
fresh() { # reset corpus/locks/spool/trace for a new scenario
  rm -rf "${KB_SESSIONS_DIR:?}" "${KB_CAPTURE_LOCKS:?}" "${KB_CAPTURE_SPOOL:?}" "${TMPROOT:?}/work"
  mkdir -p "$KB_SESSIONS_DIR" "$TMPROOT/work"
  rm -f "$KB_CAPTURE_TRACE" "$KB_LOG" "$KB_FAIL_FLAG" "$KB_DROP_FAIL" "$KB_HANGING" "$KB_CALLS"
  : >"$KB_CAPTURE_TRACE"; : >"$KB_LOG"
  unset KB_REAL_CAPTURE KB_HANG_AT KB_EMIT_SLEEP KB_CHAIN_SLEEP KB_SCAN_SLEEP KB_NO_PLANNER KB_CAPTURE_SEG_MAX_PART_BYTES KB_CAPTURE_SEG_MIN_TARGET KB_CAPTURE_SEG_SPOOL_MAX
  export KB_CAPTURE_SEGMENTS=1
  SEQ=$((SEQ + 1))
  SID="5e600000-0000-4000-8000-$(printf '%012d' "$SEQ")"
  S="$TMPROOT/work/2026-08-24T10-00-00-000Z_$SID.jsonl"
}
hook_input() { printf '{"session_file":"%s","session_id":"%s","cwd":"%s"}' "$1" "$2" "$TMPROOT"; }
hook_fg() { hook_input "$S" "$SID" | bash "$CAPTURE" >"$TMPROOT/last.out" 2>"$TMPROOT/last.err"; }
HOOK_N=0
hook_bg() {
  HOOK_N=$((HOOK_N + 1))
  hook_input "$S" "$SID" >"$TMPROOT/in.$HOOK_N"
  bash "$CAPTURE" <"$TMPROOT/in.$HOOK_N" >"$TMPROOT/bg.out" 2>"$TMPROOT/bg.err" &
  LAST_PID=$!
  STARTED_PIDS+=("$LAST_PID")
}
wait_for() { local n=$(($1 * 20)) i; shift; for ((i = 0; i < n; i++)); do "$@" && return 0; sleep 0.05; done; return 1; }
html_of() { ls "$KB_SESSIONS_DIR"/session-????????T??????Z-"$1".html 2>/dev/null | head -1; }
# the transcript inside a capture html (the first <pre>, unescaped)
pre_of() { python3 -c 'import html,re,sys
s=open(sys.argv[1]).read(); m=re.search(r"<pre>(.*?)</pre>", s, re.S); sys.stdout.write(html.unescape(m.group(1)) if m else "")' "$1"; }
part_ids() { ls "$KB_SESSIONS_DIR" 2>/dev/null | sed -n 's/^session-[0-9TZ]*-\(.*\)\.html$/\1/p' | LC_ALL=C sort; }
n_parts() { part_ids | wc -l | tr -d ' '; }
pid_of() { if [ "$1" -le 1 ]; then printf '%s' "$SID"; else printf '%s-p%02d' "$SID" "$1"; fi; }
tcount() { grep -c -- "$1" "$KB_CAPTURE_TRACE" 2>/dev/null || true; }
lock_free() { local f; for f in "$KB_CAPTURE_LOCKS"/*.lock; do [ -e "$f" ] || continue; ( flock -n 8 ) 8>"$f" || return 1; done; return 0; }
no_scratch() { ! ls -d "$TMPDIR"/tmp.* >/dev/null 2>&1; }
# every frozen part (state=frozen) must have been converted at most once
frozen_dups() { grep '^convert .*state=frozen' "$KB_CAPTURE_TRACE" | sort | uniq -d; }
tab_rows() { cat "$KB_CAPTURE_LOCKS"/*.seg 2>/dev/null | wc -l | tr -d ' '; }
unescaped_all() { local id f; for id in $(part_ids); do f="$(html_of "$id")"; pre_of "$f"; done; }

# ---------------------------------------------------------------------------
# 0. The flag, the probe and the lib helpers.
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
unset KB_CAPTURE_SEGMENTS
hook_fg
if [ "$(n_parts)" = 1 ] && [ -n "$(html_of "$SID")" ]; then
  ok "flag off (no env, no flag file): the long session lands as ONE capture"
else
  bad "flag off still segmented: $(part_ids | tr '\n' ' ')"
fi
fresh
unset KB_CAPTURE_SEGMENTS
mkdir -p "$XDG_CONFIG_HOME/kb" && : >"$XDG_CONFIG_HOME/kb/capture-segments"
python3 "$GEN" create "$S" "$SID" 60 --blob 300
hook_fg
if [ "$(n_parts)" -ge 3 ]; then
  ok "the flag FILE (\$XDG_CONFIG_HOME/kb/capture-segments) enables it with no env var - a running omp needs no restart"
else
  bad "flag file did not enable segmentation: $(part_ids | tr '\n' ' ')"
fi
fresh
KB_CAPTURE_SEGMENTS=0 python3 "$GEN" create "$S" "$SID" 60 --blob 300
KB_CAPTURE_SEGMENTS=0 hook_fg
if [ "$(n_parts)" = 1 ]; then
  ok "KB_CAPTURE_SEGMENTS=0 overrides the flag file"
else
  bad "KB_CAPTURE_SEGMENTS=0 did not win over the flag file"
fi
rm -rf "${XDG_CONFIG_HOME:?}/kb"

fresh
export KB_NO_PLANNER=1
python3 "$GEN" create "$S" "$SID" 60 --blob 300
hook_fg
warns="$(grep -c "has no 'sessions segment-plan'" "$TMPROOT/last.err" 2>/dev/null || true)"
unset KB_NO_PLANNER
if [ "$(n_parts)" = 1 ] && [ "$warns" = 1 ]; then
  ok "a kb without segment-plan: ONE stderr warning per run, the single-capture path lands the session"
else
  bad "missing-planner fallback wrong (parts=$(n_parts) warns=$warns)"
fi

# lib helpers
fresh
mkdir -p "$KB_CAPTURE_SPOOL"
. "$HOOKS_DIR/kb-hook-lib.sh"
for i in 02 03 04; do printf 'session_id=GRP-p%s\n' "$i" >"$KB_CAPTURE_SPOOL/GRP-p$i.meta"; done
printf 'session_id=GRP\n' >"$KB_CAPTURE_SPOOL/GRP.meta"
printf 'session_id=GRP-pab\n' >"$KB_CAPTURE_SPOOL/x1.meta"
printf 'session_id=GRPX-p02\n' >"$KB_CAPTURE_SPOOL/x2.meta"
printf 'session_id=OTHER\n' >"$KB_CAPTURE_SPOOL/OTHER.meta"
if [ "$(hook_spool_count_group GRP)" = 4 ] && [ "$(hook_spool_count_group NONE)" = 0 ]; then
  ok "hook_spool_count_group counts the bare id plus <id>-p<NN> parts only"
else
  bad "hook_spool_count_group = $(hook_spool_count_group GRP) (want 4)"
fi

# ---------------------------------------------------------------------------
# 1. Below the threshold the single-capture path is BYTE-IDENTICAL (golden made
#    by the pre-segmentation script), flag on or off.
for flagval in 0 1; do
  fresh
  S="$TMPROOT/work/SRC.jsonl"
  cp "$FIX/omp-session-ok4.jsonl" "$S"
  export KB_CAPTURE_SEGMENTS="$flagval" KB_CAPTURE_SEGMENT_BYTES=16777216
  printf '{"session_file":"%s","session_id":"","cwd":"/tmp"}' "$S" | bash "$CAPTURE" >/dev/null 2>&1
  got="$(sed -e "s#$S#@SRC@#g" "$KB_SESSIONS_DIR"/session-*.html)"
  if [ "$got" = "$(cat "$FIX/omp-session-ok4.single-capture.golden.html")" ]; then
    ok "below the threshold the capture is byte-identical to the golden (KB_CAPTURE_SEGMENTS=$flagval)"
  else
    bad "below-threshold capture differs from the golden (KB_CAPTURE_SEGMENTS=$flagval)"
  fi
  if [ "$flagval" = 1 ] && ! grep -q 'convert part=' "$KB_CAPTURE_TRACE"; then
    ok "a one-part plan never enters the segmented path (no part conversion traced)"
  elif [ "$flagval" = 1 ]; then
    bad "a one-part plan took the segmented path"
  fi
done
export KB_CAPTURE_SEGMENT_BYTES=6000

# ---------------------------------------------------------------------------
# 2. A long session: parts land, ids/links are right, each frozen part is
#    converted exactly once across appended turns.
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300 --writes 7
hook_fg
N1="$(n_parts)"
if [ "$N1" -ge 4 ] && [ -n "$(html_of "$SID")" ] && [ -n "$(html_of "$(pid_of 2)")" ]; then
  ok "a long session lands as $N1 parts: the bare id plus <id>-pNN (catch-up ran to completion in one invocation)"
else
  bad "expected >=4 parts, got: $(part_ids | tr '\n' ' ')"
fi
# every record of part k carries its part id; the first meta of k>=2 links back
links_ok=1
for k in $(seq 2 "$N1"); do
  f="$(html_of "$(pid_of "$k")")"
  [ -n "$f" ] || { links_ok=0; break; }
  body="$(pre_of "$f")"
  first="$(printf '%s\n' "$body" | head -1)"
  want="$(pid_of "$k")"
  if [ "$(printf '%s' "$first" | jq -r '[.type, .sessionId, .segmentOf, .segmentIdx, .rawSessionId] | @tsv')" != "$(printf 'adapter-meta\t%s\t%s\t%s\t%s' "$want" "$SID" "$k" "$SID")" ]; then
    links_ok=0; echo "# part $k first record: $first" >&2
  fi
  if [ "$(printf '%s\n' "$body" | jq -r '.sessionId' | sort -u)" != "$want" ]; then
    links_ok=0; echo "# part $k carries foreign sessionIds" >&2
  fi
done
body1="$(pre_of "$(html_of "$SID")")"
if [ "$links_ok" = 1 ] && [ "$(printf '%s\n' "$body1" | head -1 | jq -r 'has("segmentOf")')" = false ] \
  && [ "$(printf '%s\n' "$body1" | jq -r '.sessionId' | sort -u)" = "$SID" ]; then
  ok "every record of part k>=2 carries <id>-pNN; its first adapter-meta has segmentOf/segmentIdx/rawSessionId; part 1 carries no keys"
else
  bad "part ids / segment links are wrong"
fi
if [ -z "$(frozen_dups)" ] && [ "$(tcount '^convert part=.*state=frozen')" -ge $((N1 - 1)) ]; then
  ok "each frozen part was converted exactly once"
else
  bad "frozen part conversions: $(grep '^convert' "$KB_CAPTURE_TRACE" | tr '\n' ' ')"
fi
if [ "$(tab_rows)" = "$N1" ] && lock_free && no_scratch; then
  ok "the landed table has one row per part; lock released and scratch removed"
else
  bad "table rows=$(tab_rows) (want $N1) or lock/scratch leaked"
fi
# the edited-set snapshot rides the part that holds the write tool calls
snaps=0
for id in $(part_ids); do pre_of "$(html_of "$id")" | grep -q '"file-history-snapshot"' && snaps=$((snaps + 1)); done
[ "$snaps" -ge 2 ] && ok "the edited-set snapshot is emitted per part (parts with Write calls: $snaps)" || bad "snapshots per part: $snaps"

# K appended turns: earlier frozen parts are not touched again.
: >"$KB_CAPTURE_TRACE"
python3 "$GEN" append "$S" 25 --blob 300
hook_fg
N2="$(n_parts)"
if [ "$N2" -gt "$N1" ] && [ -z "$(frozen_dups)" ]; then
  ok "after 25 appended turns ($N1 -> $N2 parts) no frozen part was converted twice"
else
  bad "append: parts $N1->$N2, dups: $(frozen_dups | tr '\n' ' ')"
fi
if [ "$(awk -v m=$((N1 - 1)) '/^convert part=/ { split($2, a, "="); if (a[2] + 0 <= m) c++ } END { print c + 0 }' "$KB_CAPTURE_TRACE")" = 0 ]; then
  ok "the parts that were already frozen before the append were not reconverted at all"
else
  bad "old frozen parts reconverted: $(grep '^convert' "$KB_CAPTURE_TRACE" | tr '\n' ' ')"
fi

# one more turn: only the live tail is converted and landed
: >"$KB_CAPTURE_TRACE"
python3 "$GEN" append "$S" 1 --blob 300
hook_fg
conv="$(grep '^convert' "$KB_CAPTURE_TRACE" | sed 's/ state=.*//' | sort -u | tr '\n' ' ')"
if [ "$(grep -c '^convert' "$KB_CAPTURE_TRACE")" -le 2 ] && [ "$(grep -c '^land' "$KB_CAPTURE_TRACE")" -le 2 ]; then
  ok "one appended turn converts/lands O(1) parts (converted: $conv)"
else
  bad "one appended turn: $(grep -E '^(convert|land)' "$KB_CAPTURE_TRACE" | tr '\n' ' ')"
fi

# Part 1 is re-landed SMALLER at its first freeze: a session captured whole
# before segmentation keeps its artifact (same file) and loses the later turns
# to the -pNN parts.
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
KB_CAPTURE_SEGMENTS=0 hook_fg
whole="$(html_of "$SID")"; whole_size="$(stat -c %s "$whole")"
rm -f "$KB_CAPTURE_LOCKS"/*.done
hook_fg
if [ "$(html_of "$SID")" = "$whole" ] && [ "$(stat -c %s "$whole")" -lt "$whole_size" ] \
  && pre_of "$whole" | grep -q 'TURN-t00001' && ! pre_of "$whole" | grep -q 'TURN-t00060' && [ "$(n_parts)" -ge 4 ]; then
  ok "an already-captured whole session: the bare-id artifact is re-landed in place, smaller ($whole_size -> $(stat -c %s "$whole") bytes), the rest arrives as -pNN"
else
  bad "part 1 re-land: same file=$([ "$(html_of "$SID")" = "$whole" ] && echo y || echo n) size $whole_size -> $(stat -c %s "$whole") parts=$(n_parts)"
fi

# ---------------------------------------------------------------------------
# 3. The concatenated parts equal the single-capture translation (single model).
fresh
python3 "$GEN" create "$S" "$SID" 45 --blob 300
S_FULL="$S"
hook_fg
parts_cat="$TMPROOT/parts.cat"
: >"$parts_cat"
for id in $(part_ids); do
  f="$(html_of "$id")"
  if [ "$id" = "$SID" ]; then pre_of "$f"; else pre_of "$f" | tail -n +2; fi
done | jq -c 'del(.sessionId)' >"$parts_cat"
mkdir -p "$TMPROOT/legacy"
rm -rf "${KB_SESSIONS_DIR:?}"; mkdir -p "$KB_SESSIONS_DIR" "${KB_CAPTURE_LOCKS:?}"; rm -rf "${KB_CAPTURE_LOCKS:?}"/*
KB_CAPTURE_SEGMENTS=0 hook_fg
pre_of "$(html_of "$SID")" | jq -c 'del(.sessionId)' >"$TMPROOT/legacy.cat"
if cmp -s "$parts_cat" "$TMPROOT/legacy.cat"; then
  ok "the parts concatenated (continuation adapter-meta dropped, as 'recover --chain' does) equal the single-capture translation record for record"
else
  bad "concatenated parts differ from the legacy translation ($(wc -l <"$parts_cat") vs $(wc -l <"$TMPROOT/legacy.cat") records)"
  diff <(head -c 3000 "$parts_cat") <(head -c 3000 "$TMPROOT/legacy.cat") | head -5 >&2
fi

# ---------------------------------------------------------------------------
# 4. A title-only change re-lands ONLY the tail.
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
hook_fg
Nt="$(n_parts)"
: >"$KB_CAPTURE_TRACE"
python3 - "$S" <<'PY'
import sys, json
p = sys.argv[1]
b = open(p, "rb").read().split(b"\n")
old = b[1]
new = json.dumps({"type": "title", "v": 1, "title": "Renamed in place"}).encode()
b[1] = new + b" " * (len(old) - len(new))
assert len(b[1]) == len(old)
open(p, "wb").write(b"\n".join(b))
PY
sleep 0.05
hook_fg
landed_ids="$(grep '^land' "$KB_CAPTURE_TRACE" | sed 's/ rc=.*//' | sort -u | tr '\n' ' ')"
if [ "$(grep -c '^land' "$KB_CAPTURE_TRACE")" = 1 ] && [ "$(grep -c '^convert' "$KB_CAPTURE_TRACE")" = 1 ] \
  && [ "$landed_ids" = "land part=$Nt " ]; then
  ok "a title-only change converts and lands only the live tail (part $Nt of $Nt)"
else
  bad "title change touched: $(grep -E '^(convert|land)' "$KB_CAPTURE_TRACE" | tr '\n' ' ')"
fi
pre_of "$(html_of "$(pid_of "$Nt")")" | head -1 | jq -e '.aiTitle == "Renamed in place"' >/dev/null \
  && ok "...and the tail carries the new title" || bad "the tail does not carry the renamed title"

# ---------------------------------------------------------------------------
# 5. rc 1 / rc 2 never record landed; the retry does not reconvert.
for mode in spooled lost; do
  fresh
  python3 "$GEN" create "$S" "$SID" 60 --blob 300
  : >"$KB_FAIL_FLAG"
  [ "$mode" = lost ] && export KB_CAPTURE_SPOOL="/dev/null/nospool"
  hook_fg
  rows="$(tab_rows)"; landed_html="$(n_parts)"
  conv1="$(grep -c '^convert' "$KB_CAPTURE_TRACE")"
  if [ "$rows" = 0 ] && [ "$landed_html" = 0 ] && [ "$conv1" = 1 ] && grep -q '^land part=[0-9]* rc=[12]$' "$KB_CAPTURE_TRACE"; then
    ok "[$mode] a failed landing records nothing as landed (rc $(grep -o 'rc=[12]' "$KB_CAPTURE_TRACE" | head -1)), the pass stops after one conversion"
  else
    bad "[$mode] failed landing: rows=$rows html=$landed_html conv=$conv1 $(grep '^land' "$KB_CAPTURE_TRACE" | tr '\n' ' ')"
  fi
  rm -f "$KB_FAIL_FLAG"
  export KB_CAPTURE_SPOOL="$TMPROOT/spool"
  hook_fg
  Nm="$(n_parts)"
  firstconv="$(grep '^convert' "$KB_CAPTURE_TRACE" | head -1 | sed 's/ state=.*//')"
  if [ "$Nm" -ge 4 ] && [ "$(grep -c "^$firstconv " "$KB_CAPTURE_TRACE")" = 1 ]; then
    ok "[$mode] the retry lands the cached conversion without converting it again ($firstconv converted once), then catches up"
  else
    bad "[$mode] retry: parts=$Nm, conversions of the failed part: $(grep -c "^$firstconv " "$KB_CAPTURE_TRACE")"
  fi
done

# ---------------------------------------------------------------------------
# 6. kill -9 mid catch-up: landed parts survive, the next pass resumes.
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
export KB_HANG_AT=3
hook_bg; KP=$LAST_PID
if wait_for 20 test -e "$KB_HANGING"; then
  landed_before="$(part_ids | tr '\n' ' ')"
  sums_before="$(for id in $(part_ids); do sha256sum "$(html_of "$id")"; done | sha256sum)"
  rows_before="$(tab_rows)"
  kill -KILL "$KP" 2>/dev/null
  wait "$KP" 2>/dev/null
  gone=0
  for _ in $(seq 1 200); do lock_free && { gone=1; break; }; sleep 0.05; done
  left="$(ps -s "$KP" -o pid= 2>/dev/null | tr -d ' ' | tr '\n' ' ')"
  hp="$(cat "$KB_HANGING" 2>/dev/null)"
  if [ "$gone" = 1 ] && [ -z "$(echo "$left" | tr -d ' ')" ] && ! kill -0 "$hp" 2>/dev/null; then
    ok "kill -9 mid catch-up: the watchdog reaps the hung kb/jq descendants and frees the lock"
  else
    bad "kill -9: lock free=$gone, leftovers='$left', hung kb alive=$(kill -0 "$hp" 2>/dev/null && echo yes || echo no)"
    kill -KILL "$hp" 2>/dev/null
  fi
  if [ "$landed_before" = "$(part_ids | tr '\n' ' ')" ] && [ "$sums_before" = "$(for id in $(part_ids); do sha256sum "$(html_of "$id")"; done | sha256sum)" ] \
    && [ "$rows_before" -ge 2 ]; then
    ok "...the parts landed before the kill are intact ($rows_before recorded)"
  else
    bad "landed parts changed across the kill"
  fi
  unset KB_HANG_AT
  rm -f "$KB_HANGING"
  : >"$KB_CAPTURE_TRACE"
  hook_fg
  Nk="$(n_parts)"
  reconv="$(grep '^convert' "$KB_CAPTURE_TRACE" | sed 's/ state=.*//' | sort | uniq -d | tr '\n' ' ')"
  if [ "$Nk" -ge 4 ] && [ "$(tab_rows)" = "$Nk" ] && [ -z "$reconv" ]; then
    ok "...and the next pass resumes: every part landed ($Nk), none of the already-landed parts was converted again"
  else
    bad "resume: parts=$Nk rows=$(tab_rows) dup conversions: $reconv; trace: $(grep -E '^(convert|land)' "$KB_CAPTURE_TRACE" | tr '\n' ' ')"
  fi
  [ -z "$(grep '^convert' "$KB_CAPTURE_TRACE" | sed 's/ state=.*//' | sort | uniq -d)" ] || true
else
  bad "the hung landing never started"
  unset KB_HANG_AT
fi

# ---------------------------------------------------------------------------
# 7. Rewind / fork behind a frozen boundary: re-land in place, delete orphans.
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
hook_fg
Nf="$(n_parts)"
# fork off an early turn (inside part 2) and add a short new branch
fork_parent="t00012r"
python3 "$GEN" append "$S" 4 --parent "$fork_parent" --tag f --blob 300
: >"$KB_CAPTURE_TRACE"
hook_fg
Nafter="$(n_parts)"
orph="$(grep -c '^drop part=' "$KB_CAPTURE_TRACE")"
if [ "$Nafter" -lt "$Nf" ] && [ "$orph" = $((Nf - Nafter)) ] && [ "$(tab_rows)" = "$Nafter" ]; then
  ok "a fork behind a frozen boundary: $Nf -> $Nafter parts, $orph orphan part(s) deleted through kb ('sessions drop-part'), table matches"
else
  bad "fork: parts $Nf -> $Nafter, drops=$orph, rows=$(tab_rows)"
fi
if ! grep -q "^convert part=1 " "$KB_CAPTURE_TRACE" && pre_of "$(html_of "$(pid_of "$Nafter")")" | grep -q 'TURN-f00004'; then
  ok "...parts before the fork are untouched; the part holding the fork is re-landed in place and carries the new branch"
else
  bad "fork: wrong parts reconverted: $(grep '^convert' "$KB_CAPTURE_TRACE" | tr '\n' ' ')"
fi
leftover=0
for k in $(seq $((Nafter + 1)) "$Nf"); do [ -n "$(html_of "$(pid_of "$k")")" ] && leftover=1; done
[ "$leftover" = 0 ] && ok "...and no orphan part file is left in the corpus" || bad "an orphan part is still in the corpus: $(part_ids | tr '\n' ' ')"

# /clear: everything before the reset is hidden; the session is one part again.
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
hook_fg
Nc="$(n_parts)"
python3 "$GEN" reset "$S"
python3 "$GEN" append "$S" 2 --tag c --blob 300
: >"$KB_CAPTURE_TRACE"
hook_fg
body="$(pre_of "$(html_of "$SID")")"
if [ "$Nc" -ge 4 ] && [ "$(n_parts)" = 1 ] && printf '%s' "$body" | grep -q 'TURN-c00002' && ! printf '%s' "$body" | grep -q 'TURN-t00001' \
  && [ "$(grep -c '^drop part=' "$KB_CAPTURE_TRACE")" = $((Nc - 1)) ] && [ "$(tab_rows)" = 0 ]; then
  ok "/clear: the bare id is re-landed in place with only the post-clear chain and the $((Nc - 1)) stale parts are deleted through kb"
else
  bad "/clear: parts $Nc -> $(n_parts), drops=$(grep -c '^drop part=' "$KB_CAPTURE_TRACE"), rows=$(tab_rows)"
fi

# 7b. A FAILED drop-part is retried by the next UNCHANGED trigger: the input is
#     not recorded as done while a drop is outstanding (else the fingerprint
#     shortcut would leave the orphan in the corpus forever).
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
hook_fg
Nf2="$(n_parts)"
done_before="$(cat "$KB_CAPTURE_LOCKS"/*.done 2>/dev/null)"
python3 "$GEN" append "$S" 4 --parent t00012r --tag f --blob 300
: >"$KB_DROP_FAIL"
hook_fg
N_failed="$(n_parts)"
rows_failed="$(tab_rows)"
done_after_fail="$([ "$(cat "$KB_CAPTURE_LOCKS"/*.done 2>/dev/null)" = "$done_before" ] && echo 0 || echo 1)"
rm -f "$KB_DROP_FAIL"
: >"$KB_CAPTURE_TRACE"
hook_fg # same source, nothing appended
N_retry="$(n_parts)"
if [ "$N_failed" = "$Nf2" ] && [ "$done_after_fail" = 0 ] && [ "$N_retry" -lt "$Nf2" ] \
  && [ "$(tab_rows)" = "$N_retry" ] && [ "$(grep -c '^drop part=' "$KB_CAPTURE_TRACE")" -ge 1 ] \
  && [ -n "$done_before" ] && [ "$(cat "$KB_CAPTURE_LOCKS"/*.done 2>/dev/null)" != "$done_before" ]; then
  ok "segmented fork: a failed drop-part leaves the input unrecorded (no .done, $rows_failed rows kept); the next UNCHANGED trigger retries it ($Nf2 -> $N_retry parts) and then records"
else
  bad "segmented drop retry: parts $Nf2 -> $N_failed -> $N_retry, .done advanced after failure=$done_after_fail, rows=$(tab_rows), err=$(head -c 200 "$TMPROOT/last.err")"
fi

# 7c. Same through the legacy /clear path (the session is one part again).
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
hook_fg
Nc2="$(n_parts)"
done_before="$(cat "$KB_CAPTURE_LOCKS"/*.done 2>/dev/null)"
python3 "$GEN" reset "$S"
python3 "$GEN" append "$S" 2 --tag c --blob 300
: >"$KB_DROP_FAIL"
hook_fg
Nc_failed="$(n_parts)"
done_after_fail="$([ "$(cat "$KB_CAPTURE_LOCKS"/*.done 2>/dev/null)" = "$done_before" ] && echo 0 || echo 1)"
rm -f "$KB_DROP_FAIL"
hook_fg
if [ "$Nc2" -ge 4 ] && [ "$Nc_failed" -gt 1 ] && [ "$done_after_fail" = 0 ] && [ "$(n_parts)" = 1 ] \
  && [ -n "$done_before" ] && [ "$(cat "$KB_CAPTURE_LOCKS"/*.done 2>/dev/null)" != "$done_before" ]; then
  ok "/clear: a failed drop-part is not recorded as done ($Nc_failed parts left); the next unchanged trigger drops the stale parts (-> $(n_parts) part) and records"
else
  bad "/clear drop retry: parts $Nc2 -> $Nc_failed -> $(n_parts), .done advanced after failure=$done_after_fail"
fi

# 7d. TERM while the tail's exit scan / chain walk is slow: handled promptly
#     (the answer rides a tracked background job, never a $(...) that would
#     defer the trap), nothing of ours survives, and the next capture recovers.
term_slow() { # <label> : the slow stage is already armed via env
  local KT members gone left marker
  hook_bg; KT=$LAST_PID
  if wait_for 20 grep -q "^$2 " "$KB_LOG"; then
    members="$(ps -s "$KT" -o pid= | tr -d ' ' | grep -vx "$KT" | tr '\n' ' ')"
    kill -TERM "$KT"
    gone=0
    for _ in $(seq 1 120); do kill -0 "$KT" 2>/dev/null || { gone=1; break; }; sleep 0.05; done
    wait "$KT" 2>/dev/null
    left=""
    for p in $members; do kill -0 "$p" 2>/dev/null && left="$left $p"; done
    left="$left $(ps -s "$KT" -o pid= 2>/dev/null | tr -d ' ' | tr '\n' ' ')"
    if [ "$gone" = 1 ] && [ -n "$(echo "$members" | tr -d ' ')" ] && [ -z "$(echo "$left" | tr -d ' ')" ] && lock_free && no_scratch; then
      ok "SIGTERM during the slow $1: exits within 6 s, no owned descendant ($(echo "$members" | wc -w) processes) survives, lock released, scratch removed"
    else
      bad "TERM during $1: gone=$gone members='$members' left='$left'"
    fi
  else
    bad "the slow $1 never started"
  fi
}
mkdir -p "$TMPROOT/shim"
cat >"$TMPROOT/shim/grep" <<'GS'
#!/usr/bin/env bash
case "$*" in *session_exit*) if [ -n "${KB_SCAN_SLEEP:-}" ]; then echo "scan $$" >>"$KB_LOG"; sleep "$KB_SCAN_SLEEP"; fi ;; esac
exec "$(PATH="${PATH#"$TMPROOT/shim:"}" command -v grep)" "$@"
GS
chmod +x "$TMPROOT/shim/grep"
export TMPROOT
for stage in exit-scan chain-walk; do
  fresh
  python3 "$GEN" create "$S" "$SID" 30 --blob 300
  python3 "$GEN" exit "$S" signal
  python3 "$GEN" append "$S" 30 --blob 300
  case "$stage" in
    exit-scan) export KB_SCAN_SLEEP=60; mk=scan ;;
    chain-walk) export KB_CHAIN_SLEEP=60; mk=chain ;;
  esac
  OLDPATH="$PATH"; export PATH="$TMPROOT/shim:$PATH"
  term_slow "$stage" "$mk"
  export PATH="$OLDPATH"
  unset KB_SCAN_SLEEP KB_CHAIN_SLEEP
  hook_fg
  [ "$(n_parts)" -ge 3 ] && ok "recovery after the TERM during the $stage: the next capture lands every part" || bad "no recovery after TERM during $stage ($(n_parts) parts)"
done

# ---------------------------------------------------------------------------
# 8. A sidecar change re-lands only the part it belongs to.
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
mkdir -p "${S%.jsonl}"
# first timestamp inside the 3rd part's range: turn 12 is t=120s
sc="${S%.jsonl}/Explorer.jsonl"
printf '{"type":"session","version":3,"id":"agent-x","timestamp":"2026-08-24T10:02:30.000Z","cwd":"/tmp"}\n' >"$sc"
printf '{"type":"message","id":"a1","parentId":null,"timestamp":"2026-08-24T10:02:31.000Z","message":{"role":"user","content":[{"type":"text","text":"sub work"}]}}\n' >>"$sc"
hook_fg
Ns="$(n_parts)"
holder="$(grep '^capture ' "$KB_LOG" | grep 'sidecars=agent-Explorer.jsonl' | sed 's/^capture \([^ ]*\) .*/\1/' | sort -u | tr '\n' ' ')"
nholders="$(echo "$holder" | wc -w)"
if [ "$nholders" = 1 ]; then
  ok "the sidecar is staged into exactly ONE part ($holder)"
else
  bad "sidecar landed with: '$holder'"
fi
: >"$KB_CAPTURE_TRACE"; : >"$KB_LOG"
printf '{"type":"message","id":"a2","parentId":"a1","timestamp":"2026-08-24T10:02:40.000Z","message":{"role":"user","content":[{"type":"text","text":"more sub work"}]}}\n' >>"$sc"
sleep 0.05
hook_fg
relanded="$(grep '^capture ' "$KB_LOG" | sed 's/^capture \([^ ]*\) .*/\1/' | sort -u | tr '\n' ' ')"
if [ "$(echo "$relanded" | wc -w)" = 1 ] && [ "$(echo "$relanded" | tr -d ' ')" = "$(echo "$holder" | tr -d ' ')" ]; then
  ok "a sidecar-only change re-lands only its own part ($relanded), not the tail or the other frozen parts"
else
  bad "sidecar change re-landed: '$relanded' (holder '$holder')"
fi

# ---------------------------------------------------------------------------
# 9. Size safety: a translated part over the cap halves the target and re-plans;
#    nothing oversized is ever landed or truncated.
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
export KB_CAPTURE_SEG_MAX_PART_BYTES=4000 KB_CAPTURE_SEG_MIN_TARGET=500
hook_fg
maxlanded="$(grep '^capture ' "$KB_LOG" | sed 's/.*bytes=\([0-9]*\).*/\1/' | sort -n | tail -1)"
if grep -q '^halve target=' "$KB_CAPTURE_TRACE" && [ "${maxlanded:-0}" -le 4000 ] && [ "$(n_parts)" -ge 6 ]; then
  ok "a part over the size cap halves the target and re-plans (target $(grep -o 'target=[0-9]*' "$KB_CAPTURE_TRACE" | tail -1)); the largest landed part is $maxlanded bytes <= cap; $(n_parts) parts"
else
  bad "size safety: halve=$(grep -c '^halve' "$KB_CAPTURE_TRACE") maxlanded=$maxlanded parts=$(n_parts) trace=$(head -c 400 "$KB_CAPTURE_TRACE" | tr '\n' ' ') err=$(head -c 300 "$TMPROOT/last.err")"
fi
unset KB_CAPTURE_SEG_MAX_PART_BYTES KB_CAPTURE_SEG_MIN_TARGET

# ---------------------------------------------------------------------------
# 10. Spool cap: with 8 of the session's parts already parked, a part that is
#     not parked is NOT landed (nothing advances) instead of parking a 9th.
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
mkdir -p "$KB_CAPTURE_SPOOL"
for i in 02 03 04 05 06 07 08 09; do
  printf 'session_id=%s-p%s\n' "$SID" "$i" >"$KB_CAPTURE_SPOOL/$SID-p$i.meta"
  printf '{}\n' >"$KB_CAPTURE_SPOOL/$SID-p$i.jsonl"
done
: >"$KB_FAIL_FLAG"
hook_fg
spooled_now="$(ls "$KB_CAPTURE_SPOOL"/*.meta | wc -l | tr -d ' ')"
if [ "$spooled_now" = 8 ] && [ "$(tab_rows)" = 0 ] && grep -q 'already parked in the spool' "$TMPROOT/last.err"; then
  ok "8 parked parts: the ninth is refused (stderr says so), nothing is parked or recorded"
else
  bad "spool cap: parked=$spooled_now rows=$(tab_rows) err=$(head -c 200 "$TMPROOT/last.err")"
fi
rm -f "$KB_FAIL_FLAG"

# ---------------------------------------------------------------------------
# 11. Lifecycle: TERM mid-planner-emit reaps planner/jq/awk descendants.
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
export KB_EMIT_SLEEP=60
hook_bg; KT=$LAST_PID
if wait_for 20 grep -q '^emit ' "$KB_LOG"; then
  members="$(ps -s "$KT" -o pid= | tr -d ' ' | grep -vx "$KT" | tr '\n' ' ')"
  kill -TERM "$KT"
  gone=0
  for _ in $(seq 1 120); do kill -0 "$KT" 2>/dev/null || { gone=1; break; }; sleep 0.05; done
  wait "$KT" 2>/dev/null
  left=""
  for p in $members; do kill -0 "$p" 2>/dev/null && left="$left $p"; done
  left="$left $(ps -s "$KT" -o pid= 2>/dev/null | tr -d ' ' | tr '\n' ' ')"
  if [ "$gone" = 1 ] && [ -n "$(echo "$members" | tr -d ' ')" ] && [ -z "$(echo "$left" | tr -d ' ')" ] && lock_free && no_scratch; then
    ok "SIGTERM mid planner-emit: exits promptly, no owned descendant ($(echo "$members" | wc -w) processes) survives, lock released, scratch removed"
  else
    bad "TERM mid-emit: gone=$gone members='$members' left='$left'"
  fi
else
  bad "the planner emit never started"
fi
unset KB_EMIT_SLEEP
# the capture that follows lands everything (recovery)
hook_fg
[ "$(n_parts)" -ge 4 ] && ok "recovery after the TERM: the next capture lands every part" || bad "no recovery after TERM ($(n_parts) parts)"

# ---------------------------------------------------------------------------
# 11b. The REAL capture engine (`kb sessions capture`): the parts land with the
#      names/ids the daemon keys on, the continuation links survive the real
#      writer, and the real drop-part (when the kb has it) deletes an orphan
#      part from a REAL capture.
if [ -n "$REAL_KB" ] && "$REAL_KB" sessions capture --help 2>&1 | grep -q -- '--stamp'; then
  fresh
  export KB_REAL_CAPTURE=1
  python3 "$GEN" create "$S" "$SID" 60 --blob 300
  hook_fg
  Nr="$(n_parts)"
  p2="$(html_of "$(pid_of 2)")"
  if [ "$Nr" -ge 4 ] && [ -n "$p2" ] && grep -q "\"segmentOf\":\"$SID\"" "$p2" \
    && ! grep -q '"segmentOf"' "$(html_of "$SID")"; then
    ok "[real engine] $Nr parts land as session-<stamp>-<id>[-pNN].html; part 2 carries the segment link through the real writer, part 1 none"
  else
    bad "[real engine] parts=$Nr err=$(head -c 300 "$TMPROOT/last.err")"
  fi
  python3 "$GEN" append "$S" 4 --parent t00012r --tag f --blob 300
  hook_fg
  Nra="$(n_parts)"
  if [ "$Nra" -lt "$Nr" ] && [ "$(tab_rows)" = "$Nra" ] && [ -n "$(html_of "$SID")" ]; then
    ok "[real engine] a fork behind a frozen boundary: $Nr -> $Nra parts, the orphans are gone from the corpus (drop-part: ${REAL_DROP:-offline stand-in})"
  else
    bad "[real engine] fork: $Nr -> $Nra parts, rows=$(tab_rows), err=$(head -c 300 "$TMPROOT/last.err")"
  fi
  unset KB_REAL_CAPTURE
else
  echo "skip - this kb has no 'sessions capture --stamp': the real-engine scenario did not run"
fi

# ---------------------------------------------------------------------------
# 11c. The [session-exit] marker: legacy emits at most ONE, for the FINAL
#      session_exit. A stale non-normal exit in an earlier part (the session
#      resumed afterwards) must never become a marker of its own.
exit_chain() { # <final kind or none>
  python3 "$GEN" create "$S" "$SID" 12 --blob 300
  python3 "$GEN" exit "$S" signal
  python3 "$GEN" append "$S" 14 --blob 300
  python3 "$GEN" exit "$S" signal
  python3 "$GEN" append "$S" 14 --blob 300
  case "$1" in none) ;; *) python3 "$GEN" exit "$S" "$1" ;; esac
}
cat_parts() { # records of every part, continuation metas dropped, sessionId stripped
  local id f
  for id in $(part_ids); do
    f="$(html_of "$id")"
    if [ "$id" = "$SID" ]; then pre_of "$f"; else pre_of "$f" | tail -n +2; fi
  done | jq -c 'del(.sessionId)'
}
for final in none normal signal; do
  fresh
  exit_chain "$final"
  hook_fg
  cat_parts >"$TMPROOT/parts.cat"
  rm -rf "${KB_SESSIONS_DIR:?}" "${KB_CAPTURE_LOCKS:?}"; mkdir -p "$KB_SESSIONS_DIR"
  KB_CAPTURE_SEGMENTS=0 hook_fg
  pre_of "$(html_of "$SID")" | jq -c 'del(.sessionId)' >"$TMPROOT/legacy.cat"
  nm="$(grep -c '\[session-exit\]' "$TMPROOT/parts.cat" || true)"
  nl="$(grep -c '\[session-exit\]' "$TMPROOT/legacy.cat" || true)"
  if cmp -s "$TMPROOT/parts.cat" "$TMPROOT/legacy.cat" && [ "$nm" = "$nl" ]; then
    ok "session_exit (final=$final): the parts carry exactly the legacy marker count ($nm) and equal the single-capture translation record for record"
  else
    bad "session_exit (final=$final): markers parts=$nm legacy=$nl, records $(wc -l <"$TMPROOT/parts.cat") vs $(wc -l <"$TMPROOT/legacy.cat")"
  fi
done

# 11c2. The marker follows the RESOLVED chain, as legacy does: an exit on an
#       abandoned branch, or one at/before the last /clear, is never carried.
compare_legacy() { # <label> : S/SID already built
  hook_fg
  cat_parts >"$TMPROOT/parts.cat"
  nparts="$(n_parts)"
  rm -rf "${KB_SESSIONS_DIR:?}" "${KB_CAPTURE_LOCKS:?}"; mkdir -p "$KB_SESSIONS_DIR"
  KB_CAPTURE_SEGMENTS=0 hook_fg
  pre_of "$(html_of "$SID")" | jq -c 'del(.sessionId)' >"$TMPROOT/legacy.cat"
  nm="$(grep -c '\[session-exit\]' "$TMPROOT/parts.cat" || true)"
  nl="$(grep -c '\[session-exit\]' "$TMPROOT/legacy.cat" || true)"
  if cmp -s "$TMPROOT/parts.cat" "$TMPROOT/legacy.cat" && [ "$nm" = "$nl" ] && [ "$nparts" -ge 2 ]; then
    ok "$1: $nparts parts carry exactly the legacy marker count ($nm) and equal the single-capture translation record for record"
  else
    bad "$1: parts=$nparts markers parts=$nm legacy=$nl, records $(wc -l <"$TMPROOT/parts.cat") vs $(wc -l <"$TMPROOT/legacy.cat")"
  fi
}
fresh
python3 "$GEN" create "$S" "$SID" 40 --blob 300
python3 "$GEN" exit "$S" sigterm --parent t00010r # a dead-end branch, never continued
python3 "$GEN" append "$S" 30 --blob 300 --parent t00040r
compare_legacy "a session_exit on an abandoned branch is not a marker"
fresh
python3 "$GEN" create "$S" "$SID" 20 --blob 300
python3 "$GEN" exit "$S" sigterm # omp writes it on exit; the session is resumed...
python3 "$GEN" append "$S" 10 --blob 300
python3 "$GEN" reset "$S" # ...then /clear
python3 "$GEN" append "$S" 30 --blob 300
compare_legacy "a mid-session session_exit before a /clear is not a marker"
fresh
python3 "$GEN" create "$S" "$SID" 20 --blob 300
python3 "$GEN" reset "$S"
python3 "$GEN" append "$S" 10 --blob 300
python3 "$GEN" exit "$S" sigterm # after the /clear, on the chain: carried by the tail
python3 "$GEN" append "$S" 30 --blob 300
python3 "$GEN" exit "$S" sigterm
python3 "$GEN" append "$S" 5 --blob 300
compare_legacy "a session_exit after the /clear on the chain still follows legacy"

# ---------------------------------------------------------------------------
# 11d. A part that was PARKED in the spool but never landed has no table row:
#      when the chain shrinks (rewind, /clear) its spool item and its .parts
#      cache must still go, or a later replay publishes the ghost.
for shrink_to in 3 25; do
  fresh
  python3 "$GEN" create "$S" "$SID" 60 --blob 300
  : >"$KB_FAIL_FLAG"
  hook_fg
  rm -f "$KB_FAIL_FLAG"
  parked="$(ls "$KB_CAPTURE_SPOOL"/*.meta 2>/dev/null | wc -l | tr -d ' ')"
  python3 "$GEN" create "$S" "$SID" "$shrink_to" --blob 300
  nexp="$(kb sessions segment-plan --source "$S" --state "$TMPROOT/plan.probe" --target-bytes "$KB_CAPTURE_SEGMENT_BYTES" \
    --adapter-ver probe --no-write 2>/dev/null | jq -r '.parts | length')"
  rm -f "$TMPROOT/plan.probe"
  hook_fg
  # an unrelated session landing replays whatever the spool still holds
  OTHER="5e600000-0000-4000-8000-0000000000ff"
  OS="$TMPROOT/work/2026-08-24T10-00-00-000Z_$OTHER.jsonl"
  python3 "$GEN" create "$OS" "$OTHER" 2 --blob 100
  hook_input "$OS" "$OTHER" | bash "$CAPTURE" >/dev/null 2>&1
  spool_left="$(grep -l -- "session_id=$SID-p" "$KB_CAPTURE_SPOOL"/*.meta 2>/dev/null | wc -l | tr -d ' ')"
  ghost_ids=""
  for g in $(part_ids | grep -E -- "^$SID-p"); do
    [ "$((10#${g##*-p}))" -gt "$nexp" ] && ghost_ids="$ghost_ids $g"
  done
  stale_dirs="$(ls -d "$KB_CAPTURE_LOCKS"/*.parts/* 2>/dev/null | awk -F/ -v n="$nexp" '{ if ($NF + 0 > n) print }')"
  if [ "$parked" -ge 1 ] && [ "$spool_left" = 0 ] && [ -z "$ghost_ids" ] && [ -z "$stale_dirs" ]; then
    ok "a parked-never-landed part is dropped when the chain shrinks to $nexp part(s) (parked before: $parked): no spool item, no .parts cache, no ghost published by another session's replay"
  else
    bad "ghost part after shrink to $nexp part(s): parked=$parked spool_left=$spool_left ghosts='$ghost_ids' stale_dirs='$stale_dirs' corpus=$(part_ids | tr '\n' ' ')"
  fi
done

# 11d2. A parked part that ANOTHER session's replay publishes has no landed
#       row; the chain then shrinks to one part: the published part must go.
#      (the replay is `kb sessions capture --replay-spool`: it needs the REAL engine)
if [ -n "$REAL_KB" ] && "$REAL_KB" sessions capture --help 2>&1 | grep -q -- '--stamp'; then
  fresh
  python3 "$GEN" create "$S" "$SID" 60 --blob 300
  : >"$KB_FAIL_FLAG"
  hook_fg
  rm -f "$KB_FAIL_FLAG"
  OTHER="5e600000-0000-4000-8000-0000000000fe"
  OS="$TMPROOT/work/2026-08-24T10-00-00-000Z_$OTHER.jsonl"
  python3 "$GEN" create "$OS" "$OTHER" 2 --blob 100
  hook_input "$OS" "$OTHER" | KB_REAL_CAPTURE=1 bash "$CAPTURE" >/dev/null 2>&1
  published="$(part_ids | grep -c -E -- "^$SID-p" || true)"
  python3 "$GEN" create "$S" "$SID" 3 --blob 300
  rm -f "$KB_CAPTURE_LOCKS"/*.done
  hook_fg
  ghosts="$(part_ids | grep -E -- "^$SID-p" | tr '\n' ' ')"
  if [ "$published" -ge 1 ] && [ -z "$ghosts" ]; then
    ok "a part published by another session's replay (no landed row) is dropped when the chain shrinks to one part (published: $published)"
  else
    bad "published-parked part survived the shrink: published=$published ghosts='$ghosts' corpus=$(part_ids | tr '\n' ' ')"
  fi
else
  echo "skip - this kb has no 'sessions capture --stamp': the published-parked scenario did not run"
fi

# 11d3. A planner checkpoint temp left by a SIGKILL is swept at the next pass.
fresh
python3 "$GEN" create "$S" "$SID" 40 --blob 300
hook_fg
planf="$(ls "$KB_CAPTURE_LOCKS"/*.plan 2>/dev/null | head -1)"
if [ -n "$planf" ]; then
  : >"$planf.tmp4242"
  python3 "$GEN" append "$S" 1 --blob 300
  hook_fg
  [ ! -e "$planf.tmp4242" ] && ok "a planner checkpoint temp (<plan>.tmp<pid>) left by a kill is swept at the next pass" \
    || bad "stale planner temp survived: $(ls "$KB_CAPTURE_LOCKS" | tr '\n' ' ')"
else
  bad "no planner checkpoint found to seed the sweep test ($(ls "$KB_CAPTURE_LOCKS" | tr '\n' ' '))"
fi

# ---------------------------------------------------------------------------
# 11e. Flag switching segmented -> legacy -> segmented: the legacy path lands
#      the WHOLE session over part 1; re-enabling must re-land part 1 small.
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
hook_fg
seg_size="$(stat -c %s "$(html_of "$SID")")"
python3 "$GEN" append "$S" 1 --blob 300
KB_CAPTURE_SEGMENTS=0 hook_fg
whole_size="$(stat -c %s "$(html_of "$SID")")"
python3 "$GEN" append "$S" 2 --blob 300
hook_fg
back_size="$(stat -c %s "$(html_of "$SID")")"
if [ "$whole_size" -gt $((seg_size * 3)) ] && [ "$back_size" -le $((seg_size + seg_size / 4)) ] \
  && ! pre_of "$(html_of "$SID")" | grep -q 'TURN-t00060'; then
  ok "segmented -> legacy -> segmented: part 1 is re-landed small again ($seg_size -> $whole_size -> $back_size bytes), not left as the whole session"
else
  bad "flag switch: part 1 sizes seg=$seg_size legacy=$whole_size re-enabled=$back_size"
fi

# ---------------------------------------------------------------------------
# 11f. A landing that returns 0 but leaves no capture file is a failure: no
#      row recorded and no endless coalescing re-pass.
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
KB_ORIG_FAKE="$FAKE_CAPTURE_KB"
cat >"$TMPROOT/nofile-kb.sh" <<'NF'
#!/usr/bin/env bash
[ "${1:-}" = "sessions" ] && [ "${2:-}" = "capture" ] && { echo x >>"$KB_NOFILE_COUNT"; exit 0; }
exec bash "$KB_ORIG_FAKE" "$@"
NF
export KB_ORIG_FAKE KB_NOFILE_COUNT="$TMPROOT/nofile-count"
export FAKE_CAPTURE_KB="$TMPROOT/nofile-kb.sh"
rm -f "$KB_NOFILE_COUNT"
SECONDS=0
hook_fg
export FAKE_CAPTURE_KB="$KB_ORIG_FAKE"
if [ "$(tab_rows)" = 0 ] && [ "$(wc -l <"$KB_NOFILE_COUNT" 2>/dev/null || echo 0)" -le 2 ] && [ "$SECONDS" -lt 30 ]; then
  ok "a landing that returns 0 without writing a capture is a failure: nothing recorded, the pass stops (no re-landing loop)"
else
  bad "no-file landing: rows=$(tab_rows) landings=$(wc -l <"$KB_NOFILE_COUNT" 2>/dev/null) secs=$SECONDS"
fi

# ---------------------------------------------------------------------------
# 11g. Out of time with nothing landed must not spin the coalescing loop.
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
SECONDS=0
KB_CAPTURE_HARD_SECS=10 hook_fg
if [ "$SECONDS" -lt 20 ]; then
  ok "a hard deadline too short to convert anything ends the run (no busy re-plan loop)"
else
  bad "short hard deadline looped for $SECONDS s"
fi

# ---------------------------------------------------------------------------
# 13. v0.48 SEG-PERF: sidecar-heavy sessions must catch up.
SGEN="$FIX/gen-omp-sidecars.py"
# <part id> <sidecars landed with it> (the largest landing of each part)
holders() { grep '^capture ' "$KB_LOG" | awk '{ c = 0; n = split($0, a, "sidecars="); if (a[2] != "") c = split(a[2], b, ",") - 1; if (c > m[$2]) m[$2] = c } END { for (p in m) print p, m[p] }'; }
n_holders() { holders | awk '$2 > 0' | wc -l | tr -d ' '; }
held_by() { holders | awk -v p="$1" '$1 == p { print $2 }'; }
translates() { grep -c '^translate sidecar=' "$KB_CAPTURE_TRACE" 2>/dev/null || true; }

# 13a. omp's real layout: the TITLE SLOT is line 1, the header (with the
#      timestamp) line 2. Every sidecar used to read "no timestamp" from line 1
#      and land in the tail; they must spread over the parts they belong to.
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
python3 "$SGEN" "${S%.jsonl}" 24 --start 5 --step 25
hook_fg
Nh="$(n_parts)"
tailn="$(held_by "$(pid_of "$Nh")")"
if [ "$(n_holders)" -ge 6 ] && [ "${tailn:-0}" -le 4 ]; then
  ok "sidecars in omp's real layout (title slot first) spread over $(n_holders) of $Nh parts; the tail holds ${tailn:-0} of 24"
else
  bad "sidecar spread: holders=$(n_holders) tail=${tailn:-0} of 24 ($(holders | tr '\n' ' '))"
fi
if grep -q '^sidecars total=24 .*ts=24 .*unplaced=0' "$KB_CAPTURE_TRACE"; then
  ok "the assignment is reported: $(grep '^sidecars ' "$KB_CAPTURE_TRACE" | head -1)"
else
  bad "no/incorrect sidecar assignment report: $(grep '^sidecars' "$KB_CAPTURE_TRACE" | head -2)"
fi

# 13b. No readable timestamp anywhere: the file's mtime places it (the end time
#      of the subagent) - never "everything in the tail".
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
python3 "$SGEN" "${S%.jsonl}" 24 --start 5 --step 25 --no-timestamps
hook_fg
Nh="$(n_parts)"
tailn="$(held_by "$(pid_of "$Nh")")"
if [ "$(n_holders)" -ge 6 ] && [ "${tailn:-0}" -le 4 ] && grep -q '^sidecars total=24 .*mtime=24 .*unplaced=0' "$KB_CAPTURE_TRACE"; then
  ok "sidecars without any timestamp are placed by mtime over $(n_holders) parts (tail holds ${tailn:-0}); the report counts them: $(grep '^sidecars ' "$KB_CAPTURE_TRACE" | head -1)"
else
  bad "mtime fallback: holders=$(n_holders) tail=${tailn:-0} report=$(grep '^sidecars' "$KB_CAPTURE_TRACE" | head -1)"
fi

# 13c. The per-sidecar translation cache: an unchanged sidecar is never
#      translated again, not even when ANOTHER sidecar of its part changed.
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
python3 "$SGEN" "${S%.jsonl}" 8 --start 5 --step 1
hook_fg
first="$(translates)"
if [ "$first" = 8 ]; then
  ok "8 sidecars of one part: each translated once on the first capture"
else
  bad "first capture translated $first sidecars (want 8)"
fi
: >"$KB_CAPTURE_TRACE"
printf '{"type":"message","id":"late","parentId":"s3m2","timestamp":"2026-08-24T10:00:30.000Z","message":{"role":"user","content":[{"type":"text","text":"later"}]}}\n' >>"${S%.jsonl}/3-Agent.jsonl"
sleep 0.05
hook_fg
if [ "$(translates)" = 1 ] && [ "$(tcount '^land')" = 1 ]; then
  ok "one changed sidecar: only IT is translated again (1 of 8), and only its part is re-landed"
else
  bad "changed sidecar: translated $(translates), landed $(tcount '^land') ($(grep -E '^(translate|land)' "$KB_CAPTURE_TRACE" | tr '\n' ' '))"
fi
# a sidecar that cannot be translated is remembered, not retried every pass
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
mkdir -p "${S%.jsonl}"
printf 'not json at all\n' >"${S%.jsonl}/bad-Agent.jsonl"
hook_fg
: >"$KB_CAPTURE_TRACE"
python3 "$GEN" append "$S" 1 --blob 300
hook_fg
if [ "$(translates)" = 0 ]; then
  ok "an untranslatable sidecar is remembered (a later pass does not retry it)"
else
  bad "untranslatable sidecar retried: $(translates)"
fi

# 13d. A slow tail (many sidecars, a squeezed deadline): frozen parts still land
#      in the same pass, the translation work survives the deadline, and the
#      run converges with every sidecar translated exactly once. The bare-id
#      capture of the whole session is replaced by the small part 1.
REAL_JQ="$(command -v jq)"
mkdir -p "$TMPROOT/jqbin"
cat >"$TMPROOT/jqbin/jq" <<JQ
#!/usr/bin/env bash
# slows only a sidecar TRANSLATION (jq -s ... --arg file <sidecar>), like a big sidecar on a loaded host
case " \$* " in *" -s --arg file "*"-Agent.jsonl "*) [ -n "\${JQ_SLOW:-}" ] && sleep "\$JQ_SLOW" ;; esac
exec "$REAL_JQ" "\$@"
JQ
chmod +x "$TMPROOT/jqbin/jq"
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
python3 "$SGEN" "${S%.jsonl}" 24 --start 590 --step 0
KB_CAPTURE_SEGMENTS=0 hook_fg
whole="$(html_of "$SID")"; whole_size="$(stat -c %s "$whole")"
rm -f "${KB_CAPTURE_LOCKS:?}"/*.done
: >"$KB_CAPTURE_TRACE"
SECONDS=0
PATH="$TMPROOT/jqbin:$PATH" JQ_SLOW=0.8 KB_CAPTURE_HARD_SECS=40 hook_fg
SQ=$SECONDS
Nq="$(n_parts)"
firstland="$(grep '^land' "$KB_CAPTURE_TRACE" | head -1 | sed 's/ rc=.*//; s/land part=//')"
if [ "$Nq" -ge 4 ] && [ "$(tab_rows)" = "$Nq" ] && [ "$(translates)" = 24 ]; then
  ok "a slow 24-sidecar tail under a 40 s pass deadline converges ($Nq parts landed, every sidecar translated exactly once, $SQ s)"
else
  bad "squeezed catch-up: parts=$Nq rows=$(tab_rows) translated=$(translates) after $SQ s: $(head -c 300 "$TMPROOT/last.err")"
fi
if [ -n "$firstland" ] && [ "$firstland" != "$Nq" ]; then
  ok "frozen parts landed while the tail was still being staged (first landing: part $firstland of $Nq)"
else
  bad "the expensive tail starved the frozen parts (first landing: part ${firstland:-none}; $(grep '^land' "$KB_CAPTURE_TRACE" | tr '\n' ' '))"
fi
if [ "$(html_of "$SID")" = "$whole" ] && [ "$(stat -c %s "$whole")" -lt "$whole_size" ]; then
  ok "the stale whole-session bare-id capture was replaced by the small part 1 ($whole_size -> $(stat -c %s "$whole") bytes)"
else
  bad "bare-id capture not replaced: $whole_size -> $(stat -c %s "$(html_of "$SID")")"
fi

# 13e. The landing cap fits the part: a tail whose payload is big gets a longer
#      cap (base + payload / rate), still bounded by the pass deadline and by
#      KB_CAPTURE_LAND_MAX_SECS. (The stand-in capture sleeps 24 s when the
#      sidecar payload is over 2000 bytes: the old fixed 20 s cap killed it.)
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
python3 "$SGEN" "${S%.jsonl}" 6 --start 590 --step 0 --records 40
export KB_CAP_SLEEP_OVER=2000 KB_CAP_SLEEP=24
KB_CAPTURE_LAND_BYTES_PER_SEC=1000 hook_fg
Nl="$(n_parts)"
unset KB_CAP_SLEEP_OVER KB_CAP_SLEEP
if [ "$(tab_rows)" = "$Nl" ] && [ "$Nl" -ge 4 ] && [ -n "$(html_of "$(pid_of "$Nl")")" ]; then
  ok "a landing that outlasts the old fixed 20 s cap lands (the cap scales with the payload): $Nl parts"
else
  bad "big-payload tail did not land: rows=$(tab_rows) parts=$Nl"
fi
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
python3 "$SGEN" "${S%.jsonl}" 6 --start 590 --step 0 --records 40
export KB_CAP_SLEEP_OVER=2000 KB_CAP_SLEEP=24
SECONDS=0
KB_CAPTURE_LAND_MAX_SECS=21 KB_CAPTURE_LAND_BYTES_PER_SEC=1000 hook_fg
LS=$SECONDS
if [ -z "$(html_of "$(pid_of "$Nl")")" ] && [ "$(n_parts)" = 0 ] && [ "$LS" -lt 45 ]; then
  ok "the scaled landing cap is bounded (KB_CAPTURE_LAND_MAX_SECS): a hung tail landing is killed (after $LS s), nothing is recorded"
else
  bad "hung landing was not bounded: parts=$(n_parts) after $LS s"
fi
# the NEXT pass goes frozen-first: a tail that failed cannot starve the catch-up
: >"$KB_CAPTURE_TRACE"
KB_CAPTURE_LAND_MAX_SECS=21 KB_CAPTURE_LAND_BYTES_PER_SEC=1000 hook_fg
firstland="$(grep '^land' "$KB_CAPTURE_TRACE" | head -1 | sed 's/ rc=.*//; s/land part=//')"
if [ "$(n_parts)" = 4 ] && [ "$firstland" = 1 ] && [ -z "$(html_of "$(pid_of "$Nl")")" ] && grep -q "^land part=$Nl " "$KB_CAPTURE_TRACE"; then
  ok "after a failed tail the next pass is frozen-first (4 frozen parts, first landing: part $firstland) and the tail is still retried once"
else
  bad "frozen-first after a failed tail: parts=$(n_parts) (want $((Nl - 1))) first landing=$firstland ($(grep -E '^land' "$KB_CAPTURE_TRACE" | tr '\n' ' ' | cut -c1-300))"
fi
export KB_CAP_SLEEP_OVER=2000 KB_CAP_SLEEP=2
for _r in 1 2 3 4; do KB_CAPTURE_LAND_BYTES_PER_SEC=1000 hook_fg; done
unset KB_CAP_SLEEP_OVER KB_CAP_SLEEP
if [ "$(n_parts)" = "$Nl" ] && [ "$(tab_rows)" = "$Nl" ] && [ "$(ls "$KB_CAPTURE_LOCKS" | grep -c tailfail)" = 0 ]; then
  ok "the tail lands on the following trigger and the failure marker is cleared"
else
  bad "tail did not recover: parts=$(n_parts) rows=$(tab_rows) marker=$(ls "$KB_CAPTURE_LOCKS" | grep -c tailfail)"
fi

# 13f. A stuck FROZEN part must not starve the parts behind it nor the tail:
#      tail fails once (tailfail written), then one frozen part fails on every
#      capture. Every later pass still lands the other frozen parts and the tail.
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
touch "$KB_FAIL_FLAG"; hook_fg; rm -f "$KB_FAIL_FLAG"
Nf="$(n_parts)"
if [ "$Nf" = 0 ] && ls "$KB_CAPTURE_LOCKS" | grep -q tailfail; then :; else bad "13f setup: tail failure did not leave the marker (parts=$Nf)"; fi
export KB_FAIL_SID_SUFFIX=-p03
for _r in 1 2 3 4 5 6; do hook_fg; done
unset KB_FAIL_SID_SUFFIX
if [ -n "$(html_of "$(pid_of 1)")" ] && [ -n "$(html_of "$(pid_of 2)")" ] && [ -z "$(html_of "$(pid_of 3)")" ] \
  && [ -n "$(html_of "$(pid_of 4)")" ] && [ "$(tab_rows)" -ge 5 ] && [ -z "$(ls "$KB_CAPTURE_LOCKS" | grep tailfail)" ]; then
  ok "a permanently failing frozen part (3) does not block parts behind it or the tail (tab rows=$(tab_rows), tailfail cleared)"
else
  bad "frozen failure starved the rest: parts=$(n_parts) rows=$(tab_rows) marker=$(ls "$KB_CAPTURE_LOCKS" | grep -c tailfail)"
fi

# ---------------------------------------------------------------------------
# 12. The segmented translator is DERIVED from TRANSLATE: a drifted program
#     fails closed (single-capture path, one warning), never silently.
fresh
python3 "$GEN" create "$S" "$SID" 60 --blob 300
broken="$TMPROOT/broken-hooks"; mkdir -p "$broken"
cp "$HOOKS_DIR"/kb-hook-lib.sh "$broken/"
sed '0,/last \/\/ "omp") as \$dmodel |/s//last \/\/ "omp-drift") as $dmodel |/' "$CAPTURE" >"$broken/kb-capture-omp.sh"
hook_input "$S" "$SID" | bash "$broken/kb-capture-omp.sh" >/dev/null 2>"$TMPROOT/last.err"
if [ "$(n_parts)" = 1 ] && grep -q 'translator no longer matches' "$TMPROOT/last.err"; then
  ok "a TRANSLATE edit that breaks a substitution fails closed: warning + single-capture path"
else
  bad "drift guard: parts=$(n_parts) err=$(head -c 200 "$TMPROOT/last.err")"
fi

# ---------------------------------------------------------------------------
echo
if ! ps -ef | grep -E "kb-capture-omp.sh.*$TMPROOT" | grep -v grep >/dev/null; then
  ok "no process started by this test remains"
else
  bad "a process of ours is still running"
fi
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
