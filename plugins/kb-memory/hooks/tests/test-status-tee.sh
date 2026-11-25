#!/usr/bin/env bash
# test-status-tee.sh - STATHOOK: kb-status-tee.sh (the statusLine pass-through
# wrapper that feeds the typed `status` beat) and the floor/dedupe marker it
# shares with kb-beat.sh. Real scripts, a curl SHIM on PATH that logs every
# POST body, synthetic fixtures only.
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-status-tee.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
TEE="$HOOKS_DIR/kb-status-tee.sh"
FIX="$SCRIPT_DIR/fixtures"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-status-tee-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT
# shellcheck source=lib-sandbox.sh
. "$SCRIPT_DIR/lib-sandbox.sh"
kb_test_sandbox "$TMPROOT" || exit 1

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

for t in jq git; do
  command -v "$t" >/dev/null 2>&1 || { echo "SKIP: $t not installed"; echo "passed=0 failed=0 skipped=1"; exit 0; }
done

SHIMDIR="$TMPROOT/shim"
mkdir -p "$SHIMDIR"
export SHIM_LOG="$TMPROOT/curl.log"
: >"$SHIM_LOG"
cat >"$SHIMDIR/curl" <<'SHIM'
#!/usr/bin/env bash
while [ $# -gt 0 ]; do
  case "$1" in
    --data-binary) printf '%s' "$2" | jq -c . >>"$SHIM_LOG" 2>/dev/null; shift ;;
  esac
  shift
done
[ -z "${SHIM_SLEEP:-}" ] || sleep "$SHIM_SLEEP"
[ "${SHIM_FAIL:-}" != "1" ] || exit 7
exit 0
SHIM
chmod +x "$SHIMDIR/curl"
export PATH="$SHIMDIR:$PATH"
export KB_SESSIONS_DIR="$TMPROOT/sessions"
mkdir -p "$KB_SESSIONS_DIR"
export KB_DAEMON_URL="http://127.0.0.1:9"
unset KB_BEAT KB_BEAT_STATUS KB_STATUS_NOW KB_STATUS_MIN_INTERVAL_SECS KB_STATUS_REFRESH_SECS KB_TERM_ID

REPO="$TMPROOT/repo"
git init -q -b feat/demo "$REPO"
git -C "$REPO" -c user.email=t@example.invalid -c user.name=t commit -q --allow-empty -m init

now_ms() { date +%s%N 2>/dev/null | cut -c1-13; }
count() { wc -l <"$SHIM_LOG" | tr -d ' '; }
# wait_count <n> - poll up to 4 s for the log to reach n lines
wait_count() {
  local i
  for i in $(seq 1 40); do
    [ "$(count)" -ge "$1" ] && return 0
    sleep 0.1
  done
  return 1
}
# fixture <name> <sid> [cwd] - the synthetic statusLine JSON for a session
fixture() { jq -c --arg sid "$2" --arg d "${3:-$REPO}" \
  '.session_id=$sid | (if .workspace then .workspace.current_dir=$d else .cwd=$d end)' "$FIX/$1"; }
# tee_call <json> [env...] - one tee call wrapping `cat`; waits for the background job
tee_call() {
  local json="$1"
  shift
  printf '%s' "$json" | env "$@" "$TEE" -- cat >/dev/null
  sleep 0.5
}
last_status() { tail -1 "$SHIM_LOG" | jq -c '.status'; }

echo "== kb-status-tee.sh (STATHOOK) =="

# --- 1. pass-through: stdout bytes + exit code identical with and without ----
WRAP_OUT='printf "L1\n\n  L3 no newline"; exit 3'
input="$(fixture status-full.json s-pass)"
direct_out="$(printf '%s' "$input" | bash -c "$WRAP_OUT"; echo "rc=$?")"
tee_out="$(printf '%s' "$input" | "$TEE" -- bash -c "$WRAP_OUT"; echo "rc=$?")"
if [ "$direct_out" = "$tee_out" ] && [ "$tee_out" != "rc=0" ]; then
  ok "wrapped stdout and exit code pass through unchanged"
else
  bad "pass-through differs (direct=[$direct_out] tee=[$tee_out])"
fi

# the wrapped command gets the SAME stdin, byte for byte (trailing newlines kept)
printf '%s\n\n' "$input" >"$TMPROOT/stdin.bin"
"$TEE" -- cat <"$TMPROOT/stdin.bin" >"$TMPROOT/stdin.out"
if cmp -s "$TMPROOT/stdin.bin" "$TMPROOT/stdin.out"; then
  ok "the wrapped command reads the original stdin byte-exact"
else
  bad "stdin forwarded to the wrapped command differs"
fi

# --- 2. no wrapped command prints nothing ---------------------------------
out="$(printf '%s' "$input" | "$TEE"; echo "rc=$?")"
[ "$out" = "rc=0" ] && ok "no wrapped command: prints nothing, exit 0" || bad "no wrapped command printed: [$out]"
out="$(printf '%s' "$input" | "$TEE" --; echo "rc=$?")"
[ "$out" = "rc=0" ] && ok "a bare -- prints nothing, exit 0" || bad "bare -- printed: [$out]"
sleep 0.5

# --- 3. a slow daemon never delays the visible line --------------------------
: >"$SHIM_LOG"
start="$(now_ms)"
printf '%s' "$(fixture status-full.json s-slow)" | SHIM_SLEEP=5 "$TEE" -- true
elapsed=$(( $(now_ms) - start ))
if [ "$elapsed" -lt 500 ]; then
  ok "a beat curl sleeping 5 s adds no delay (${elapsed} ms)"
else
  bad "the tee waited on the beat (${elapsed} ms)"
fi

# --- 4. daemon down: output identical, nothing on stderr -----------------------
: >"$SHIM_LOG"
err="$(printf '%s' "$input" | SHIM_FAIL=1 "$TEE" -- bash -c 'printf VISIBLE' 2>&1 >"$TMPROOT/down.out")"
if [ "$(cat "$TMPROOT/down.out")" = "VISIBLE" ] && [ -z "$err" ]; then
  ok "daemon down: the line is unchanged and stderr is silent"
else
  bad "daemon down changed output (out=$(cat "$TMPROOT/down.out") err=$err)"
fi
sleep 0.5

# --- 5. kill switches: zero curl calls, output still passes ------------------
: >"$SHIM_LOG"
out="$(printf '%s' "$(fixture status-full.json s-off1)" | KB_BEAT=0 "$TEE" -- bash -c 'printf VISIBLE')"
out2="$(printf '%s' "$(fixture status-full.json s-off2)" | KB_BEAT_STATUS=0 "$TEE" -- bash -c 'printf VISIBLE')"
out3="$(printf '%s' "$(fixture status-full.json s-off3)" | env -u KB_SESSIONS_DIR "$TEE" -- bash -c 'printf VISIBLE')"
NOJQ="$TMPROOT/nojq"
mkdir -p "$NOJQ"
for b in cat dirname bash env; do ln -sf "$(command -v "$b")" "$NOJQ/$b"; done
ln -sf "$SHIMDIR/curl" "$NOJQ/curl"
out4="$(printf '%s' "$(fixture status-full.json s-off4)" | PATH="$NOJQ" "$TEE" -- cat 2>/dev/null | head -c 6)"
sleep 0.6
if [ "$out" = VISIBLE ] && [ "$out2" = VISIBLE ] && [ "$out3" = VISIBLE ] && [ "$out4" = '{"sess' ] && [ "$(count)" = 0 ]; then
  ok "KB_BEAT=0, KB_BEAT_STATUS=0, no sessions dir, no jq: pure pass-through, zero beats"
else
  bad "kill switches (out=$out/$out2/$out3/$out4 count=$(count))"
fi

# --- 6. derivation matrix --------------------------------------------------------
: >"$SHIM_LOG"
tee_call "$(fixture status-full.json s-full)" KB_STATUS_NOW=1000
wait_count 1
st="$(last_status)"
want='{"model":"Synthetic 1","context_pct":42,"cost_usd":1.23,"permission_mode":"plan","branch":"feat/demo"}'
[ "$(printf '%s' "$st" | jq -cS .)" = "$(printf '%s' "$want" | jq -cS .)" ] \
  && ok "full statusLine JSON derives model, ctx %, cost, mode and branch" \
  || bad "full derivation: $st"
body="$(tail -1 "$SHIM_LOG")"
if [ "$(printf '%s' "$body" | jq -r '.event')" = status ] && [ "$(printf '%s' "$body" | jq -r '.session_id')" = s-full ] \
   && [ "$(printf '%s' "$body" | jq 'has("last_line") or has("detail")')" = false ]; then
  ok "the body is a status event with no last_line or detail"
else
  bad "status body shape: $body"
fi

: >"$SHIM_LOG"
tee_call "$(fixture status-no-context.json s-noctx)" KB_STATUS_NOW=1000
wait_count 1
st="$(last_status)"
[ "$(printf '%s' "$st" | jq 'has("context_pct")')" = false ] && [ "$(printf '%s' "$st" | jq -r '.cost_usd')" = 0.5 ] \
  && ok "missing context is omitted, never zero-filled" || bad "missing context: $st"

: >"$SHIM_LOG"
tee_call "$(fixture status-bad-cost.json s-badcost)" KB_STATUS_NOW=1000
wait_count 1
st="$(last_status)"
[ "$(printf '%s' "$st" | jq 'has("cost_usd")')" = false ] && [ "$(printf '%s' "$st" | jq -r '.context_pct')" = 7 ] \
  && ok "a non-numeric cost is omitted" || bad "non-numeric cost: $st"

: >"$SHIM_LOG"
git -C "$REPO" -c user.email=t@example.invalid -c user.name=t checkout -q --detach
tee_call "$(fixture status-full.json s-detached)" KB_STATUS_NOW=1000
wait_count 1
[ "$(last_status | jq 'has("branch")')" = false ] && ok "detached HEAD omits the branch" || bad "detached HEAD: $(last_status)"
git -C "$REPO" checkout -q feat/demo

: >"$SHIM_LOG"
tee_call "$(fixture status-no-fields.json s-nofields "$TMPROOT/not-a-repo")" KB_STATUS_NOW=1000
sleep 0.5
[ "$(count)" = 0 ] && ok "no derivable field: no beat" || bad "no-field input beat anyway: $(cat "$SHIM_LOG")"
tee_call '{"workspace":{"current_dir":"/x"},"model":{"id":"m"}}' KB_STATUS_NOW=1000
[ "$(count)" = 0 ] && ok "no session_id: no beat" || bad "session-less input beat anyway"

for pair in default:default plan:plan acceptEdits:accept bypassPermissions:bypass dontAsk:other; do
  raw="${pair%%:*}"
  want="${pair##*:}"
  : >"$SHIM_LOG"
  tee_call "$(fixture status-full.json "s-pm-$raw" | jq -c --arg m "$raw" '.permission_mode=$m')" KB_STATUS_NOW=1000
  wait_count 1
  got="$(last_status | jq -r '.permission_mode')"
  [ "$got" = "$want" ] && ok "permission_mode $raw maps to $want" || bad "permission_mode $raw -> $got (want $want)"
done

# --- 7. throttle / dedupe ---------------------------------------------------------
: >"$SHIM_LOG"
j="$(fixture status-full.json s-dedupe)"
for i in 0 1 2 3 4 5 6 7 8 9; do tee_call "$j" KB_STATUS_NOW=$((2000 + i * 10)); done
[ "$(count)" = 1 ] && ok "10 identical calls send exactly 1 beat" || bad "10 identical calls sent $(count)"

: >"$SHIM_LOG"
a="$(fixture status-full.json s-held | jq -c '.context_window.used_percentage=10')"
b="$(fixture status-full.json s-held | jq -c '.context_window.used_percentage=20')"
tee_call "$a" KB_STATUS_NOW=3000
tee_call "$b" KB_STATUS_NOW=3002
n_held="$(count)"
tee_call "$b" KB_STATUS_NOW=3006
wait_count 2
if [ "$n_held" = 1 ] && [ "$(count)" = 2 ] && [ "$(last_status | jq -r '.context_pct')" = 20 ]; then
  ok "a change inside the floor is held, then sent by the next call after it"
else
  bad "floor hold (held=$n_held total=$(count))"
fi

: >"$SHIM_LOG"
tee_call "$j" KB_STATUS_NOW=4000 KB_STATUS_MIN_INTERVAL_SECS=5 KB_STATUS_REFRESH_SECS=120
tee_call "$(printf '%s' "$j" | jq -c '.session_id="s-dedupe-r"')" KB_STATUS_NOW=4000
tee_call "$(printf '%s' "$j" | jq -c '.session_id="s-dedupe-r"')" KB_STATUS_NOW=4100
n_before="$(count)"
tee_call "$(printf '%s' "$j" | jq -c '.session_id="s-dedupe-r"')" KB_STATUS_NOW=4125
wait_count 3
if [ "$n_before" = 2 ] && [ "$(count)" = 3 ]; then
  ok "an unchanged object is resent after the refresh interval"
else
  bad "refresh (before=$n_before total=$(count))"
fi

# --- 8. an unwritable cache dir still sends (fails open) -----------------------------
: >"$SHIM_LOG"
touch "$TMPROOT/afile"
printf '%s' "$(fixture status-full.json s-nocache)" | XDG_CACHE_HOME="$TMPROOT/afile" KB_STATUS_NOW=5000 "$TEE" -- cat >/dev/null
wait_count 1 && ok "an unwritable cache dir still sends" || bad "unwritable cache dir suppressed the beat"

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
