#!/usr/bin/env bash
# test-capture-omp-segments-scale.sh - the scale gate for SEGMENTED omp capture
# (v0.46 SEG-PR2): a genuinely linked synthetic chain (gen-omp-chain.py: tool
# calls with intents and results, thinking blocks, usage, compactions,
# abandoned branches, a /clear) of SEG_SCALE_MB megabytes, captured with the
# real adapter under `ulimit -v` and /usr/bin/time:
#   * catch-up      - one invocation lands every part
#   * steady state  - CPU/wall/peak RSS of ONE appended turn (the per-turn cost)
#   * no-change     - the fingerprint shortcut
# Prints `SCALE:` lines (the numbers) and asserts only loose bounds: this is a
# gate against O(session) behaviour, not a benchmark. The planner is the REAL
# one when the kb on PATH / in KB_BIN_DIR has it, else the offline stand-in
# (python: its time is reported separately and is NOT the Rust planner's).
#
#   SEG_SCALE_MB      session size (default 24)
#   SEG_SCALE_TARGET  raw bytes per part (default 4194304; production = 16 MiB)
#   SEG_SCALE_VMEM_KB `ulimit -v` in KiB (default 4194304 = 4 GiB)
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CAPTURE="$HOOKS_DIR/kb-capture-omp.sh"
FIX="$SCRIPT_DIR/fixtures"
export HOOKS_DIR
MB="${SEG_SCALE_MB:-24}"
TARGET="${SEG_SCALE_TARGET:-4194304}"
VMEM="${SEG_SCALE_VMEM_KB:-4194304}"

for need in jq flock setsid ps timeout python3 awk; do
  command -v "$need" >/dev/null 2>&1 || { echo "SKIP: $need is required"; echo "passed=0 failed=0 skipped=1"; exit 0; }
done
TIME=/usr/bin/time
[ -x "$TIME" ] || TIME=""

PASS=0; FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-omp-seg-scale.XXXXXX")"
STARTED=()
cleanup() { local p q; for p in "${STARTED[@]+"${STARTED[@]}"}"; do for q in $(ps -s "$p" -o pid= 2>/dev/null); do kill -KILL "$q" 2>/dev/null; done; done; rm -rf "${TMPROOT:?}"; }
trap cleanup EXIT

REAL_KB="$(command -v kb 2>/dev/null || true)"
[ -n "${KB_BIN_DIR:-}" ] && [ -x "$KB_BIN_DIR/kb" ] && REAL_KB="$KB_BIN_DIR/kb"
REAL_PLANNER=""
[ -n "$REAL_KB" ] && "$REAL_KB" sessions segment-plan --help >/dev/null 2>&1 && REAL_PLANNER=1
export REAL_KB REAL_PLANNER

mkdir -p "$TMPROOT/kbbin" "$TMPROOT/tmp" "$TMPROOT/home" "$TMPROOT/sessions"
export TMPDIR="$TMPROOT/tmp" HOME="$TMPROOT/home" XDG_CACHE_HOME="$TMPROOT/home/.cache" XDG_CONFIG_HOME="$TMPROOT/home/.config"
unset KB_CACHE_DIR KB_STATE_DIR KB_CONFIG_DIR
export KB_CAPTURE_LOCKS="$TMPROOT/locks" KB_CAPTURE_SPOOL="$TMPROOT/spool" KB_SESSIONS_DIR="$TMPROOT/sessions"
export KB_CAPTURE_TRACE="$TMPROOT/trace" KB_CAPTURE_SEGMENTS=1 KB_CAPTURE_SEGMENT_BYTES="$TARGET"
export PLAN_TIMES="$TMPROOT/plan.times" FAKE_CAPTURE_KB="$FIX/fake-capture-kb.sh" FAKE_PLAN="$FIX/fake-segment-plan.py"
: >"$KB_CAPTURE_TRACE"; : >"$PLAN_TIMES"

cat >"$TMPROOT/kbbin/kb" <<'KB'
#!/usr/bin/env bash
if [ "${1:-}" = "sessions" ] && [ "${2:-}" = "segment-plan" ]; then
  s=$(date +%s.%N)
  if [ -n "$REAL_PLANNER" ]; then "$REAL_KB" "$@"; rc=$?; else shift 2; python3 "$FAKE_PLAN" "$@"; rc=$?; fi
  case " $* " in *" --help "*) ;; *) echo "$(date +%s.%N) $s" | awk '{printf "%.3f\n", $1-$2}' >>"$PLAN_TIMES" ;; esac
  exit "$rc"
fi
exec bash "$FAKE_CAPTURE_KB" "$@"
KB
chmod +x "$TMPROOT/kbbin/kb"
export PATH="$TMPROOT/kbbin:$PATH"

SID="5ca1e000-0000-4000-8000-000000000001"
S="$TMPROOT/2026-08-24T10-00-00-000Z_$SID.jsonl"
LINES=$((MB * 519))
python3 "$FIX/gen-omp-chain.py" "$LINES" "$S" 11 || { bad "generator failed"; echo "passed=$PASS failed=$FAIL"; exit 1; }
SIZE="$(stat -c %s "$S")"
echo "SCALE: planner=$([ -n "$REAL_PLANNER" ] && echo real-rust || echo OFFLINE-PYTHON-STAND-IN) source=$((SIZE / 1048576)) MiB lines=$(wc -l <"$S") target=$((TARGET / 1048576)).$(((TARGET % 1048576) * 10 / 1048576)) MiB vmem-limit=$((VMEM / 1024)) MiB"

hook() { printf '{"session_file":"%s","session_id":"%s","cwd":"%s"}' "$S" "$SID" "$TMPROOT" | bash "$CAPTURE" >"$TMPROOT/hook.out" 2>"$TMPROOT/hook.err"; }
# run hook under ulimit + /usr/bin/time -> "<wall> <user+sys> <maxrss-KiB>"
timed() {
  local out="$TMPROOT/time.out"
  if [ -n "$TIME" ]; then
    ( ulimit -v "$VMEM"; "$TIME" -f '%e %U %S %M' -o "$out" bash -c 'printf "{\"session_file\":\"%s\",\"session_id\":\"%s\",\"cwd\":\"%s\"}" "$0" "$1" "$2" | bash "$3" >/dev/null 2>"$4"' "$S" "$SID" "$TMPROOT" "$CAPTURE" "$TMPROOT/hook.err" )
    awk '{printf "%.1f %.1f %d\n", $1, $2 + $3, $4}' "$out"
  else
    local t0=$SECONDS
    ( ulimit -v "$VMEM"; hook )
    echo "$((SECONDS - t0)) 0 0"
  fi
}
plan_sum() { awk '{s += $1} END {printf "%.1f", s + 0}' "$PLAN_TIMES"; }

# 1. catch-up -----------------------------------------------------------------
read -r W C R <<<"$(timed)"
NP="$(ls "$KB_SESSIONS_DIR" | wc -l | tr -d ' ')"
PT="$(plan_sum)"; NPL="$(wc -l <"$PLAN_TIMES" | tr -d ' ')"
CONV="$(grep -c '^convert' "$KB_CAPTURE_TRACE")"
echo "SCALE: catch-up      wall=${W}s cpu=${C}s peak-rss=$((R / 1024)) MiB parts=$NP conversions=$CONV planner-calls=$NPL planner-wall=${PT}s"
if [ "$NP" -ge 2 ] && [ "$CONV" -ge "$NP" ]; then ok "catch-up landed $NP parts in one invocation (peak RSS $((R / 1024)) MiB, limit $((VMEM / 1024)) MiB)"; else bad "catch-up: parts=$NP conversions=$CONV err=$(head -c 300 "$TMPROOT/hook.err")"; fi
if [ "$R" -lt $((VMEM / 2)) ]; then ok "peak RSS of the largest process stays under half the address-space limit"; else bad "peak RSS $((R / 1024)) MiB is too close to the limit"; fi

# 2. steady state: append ONE turn at a time (a valid linked turn: user +
#    assistant), then capture. The per-turn cost must not grow with the session.
append_turn() { # <n>
  local leaf
  leaf="$(tail -n 1 -- "$S" | jq -r '.id')"
  printf '{"type":"message","id":"zturn%du","parentId":"%s","timestamp":"2026-08-25T10:%02d:00.000Z","message":{"role":"user","content":[{"type":"text","text":"STEADY-%d"}]}}\n' "$1" "$leaf" "$1" "$1" >>"$S"
  printf '{"type":"message","id":"zturn%da","parentId":"zturn%du","timestamp":"2026-08-25T10:%02d:01.000Z","message":{"role":"assistant","model":"prov/model-x","content":[{"type":"text","text":"ok %d"}],"usage":{"input":10,"output":5}}}\n' "$1" "$1" "$1" "$1" >>"$S"
}
WS=(); CS=(); RS=(); PS=()
for n in 1 2 3; do
  append_turn "$n"
  : >"$PLAN_TIMES"; : >"$KB_CAPTURE_TRACE"
  read -r W C R <<<"$(timed)"
  WS+=("$W"); CS+=("$C"); RS+=("$R"); PS+=("$(plan_sum)")
  echo "SCALE: one turn #$n   wall=${W}s cpu=${C}s peak-rss=$((R / 1024)) MiB planner-wall=$(plan_sum)s conversions=$(grep -c '^convert' "$KB_CAPTURE_TRACE") landings=$(grep -c '^land' "$KB_CAPTURE_TRACE")"
done
# the last turn's trace is the per-turn work: only the tail
if [ "$(grep -c '^convert' "$KB_CAPTURE_TRACE")" -le 2 ] && [ "$(grep -c '^land' "$KB_CAPTURE_TRACE")" -le 2 ]; then
  ok "one appended turn converts/lands at most the live tail (and a part that just froze)"
else
  bad "one appended turn touched: $(grep -E '^(convert|land)' "$KB_CAPTURE_TRACE" | tr '\n' ' ')"
fi
median() { printf '%s\n' "$@" | sort -n | sed -n 2p; }
MW="$(median "${WS[@]}")"; MC="$(median "${CS[@]}")"
if awk -v t="$MW" -v c="$W" 'BEGIN { exit !(t * 2 < c || t < 5) }'; then
  ok "per-turn wall (${MW}s median) is far below the catch-up (it does not scale with the session)"
else
  bad "per-turn wall ${MW}s is not small against the catch-up"
fi
echo "SCALE: steady-state median per appended turn: wall=${MW}s cpu=${MC}s"

# 3. no change ------------------------------------------------------------------
: >"$KB_CAPTURE_TRACE"
read -r W C R <<<"$(timed)"
echo "SCALE: no-change     wall=${W}s cpu=${C}s"
if [ "$(grep -c '^convert' "$KB_CAPTURE_TRACE")" = 0 ]; then ok "an unchanged session converts nothing (fingerprint shortcut)"; else bad "unchanged session converted"; fi

echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
