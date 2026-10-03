#!/usr/bin/env bash
# test-sid-key.sh - v0.44 X10: every per-session hook file name (markers,
# throttle files, adapter capture names, spool items) derives from ONE helper,
# hook_sid_key, which never maps two distinct session ids to one name. The old
# `tr -c 'a-zA-Z0-9' '-' | cut -c1-80` form did ("a_b" and "a-b"; two ids that
# share an 80-char prefix).
#
# Runnable standalone:  bash plugins/kb-memory/hooks/tests/test-sid-key.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/kb-sid-key-test.XXXXXX")"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok      - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'not ok  - %s\n' "$1"; }

# shellcheck disable=SC1091
. "$HOOKS_DIR/kb-hook-lib.sh"

UUID="0b7e5a1c-3f4d-4e8a-9c21-5d6f7a8b9c0d"
[ "$(hook_sid_key "$UUID")" = "$UUID" ] && ok "a plain id keeps its own name" || bad "a plain id keeps its own name"

[ "$(hook_sid_key 'a_b')" != "$(hook_sid_key 'a-b')" ] && ok "a_b and a-b get distinct keys" || bad "a_b and a-b get distinct keys"

P80="$(printf 'x%.0s' $(seq 1 80))"
[ "$(hook_sid_key "${P80}1")" != "$(hook_sid_key "${P80}2")" ] && ok "ids sharing an 80-char prefix get distinct keys" || bad "ids sharing an 80-char prefix get distinct keys"

echo "== the beat heartbeat marker uses the key =="
export XDG_CACHE_HOME="$TMPROOT/cache"
export KB_SESSIONS_DIR="$TMPROOT/sessions"
export KB_BEAT_HEARTBEAT_MIN_INTERVAL_SECS=3600
mkdir -p "$KB_SESSIONS_DIR" "$XDG_CACHE_HOME"
mkdir -p "$TMPROOT/bin"
printf '#!/usr/bin/env bash\nexit 1\n' >"$TMPROOT/bin/kb"
chmod +x "$TMPROOT/bin/kb"
for id in 'a_b' 'a-b'; do
  printf '{"session_id":"%s"}' "$id" | PATH="$TMPROOT/bin:$PATH" bash "$HOOKS_DIR/kb-beat-throttle.sh" claude >/dev/null 2>&1
done
n="$(find "$XDG_CACHE_HOME/kb" -name 'beat-heartbeat-*' 2>/dev/null | wc -l | tr -d ' ')"
[ "$n" = 2 ] && ok "two distinct ids leave two distinct heartbeat markers" || bad "two distinct ids leave two distinct heartbeat markers (found $n)"

echo "== hook_spool_drop removes a matching legacy lossy-key item only =="
export KB_CAPTURE_SPOOL="$TMPROOT/spool"
mkdir -p "$KB_CAPTURE_SPOOL"
# Legacy item for "a_b" (lossy name a-b) and a colliding item that really is "a-b".
printf 'x' >"$KB_CAPTURE_SPOOL/a-b.jsonl"
printf 'session_id=a_b\n' >"$KB_CAPTURE_SPOOL/a-b.meta"
hook_spool_drop 'a_b'
[ ! -e "$KB_CAPTURE_SPOOL/a-b.jsonl" ] && ok "legacy item recorded for this exact id is dropped" || bad "legacy item recorded for this exact id is dropped"
printf 'x' >"$KB_CAPTURE_SPOOL/a-b.jsonl"
printf 'session_id=a-b\n' >"$KB_CAPTURE_SPOOL/a-b.meta"
hook_spool_drop 'a_b'
[ -e "$KB_CAPTURE_SPOOL/a-b.jsonl" ] && ok "a colliding id's item survives" || bad "a colliding id's item survives"

echo "== no hook builds a per-session name with the lossy pipeline =="
if grep -nE "tr -c 'a-zA-Z0-9' '-' \| cut -c1-80" "$HOOKS_DIR"/*.sh \
  | grep -vE 'kb-hook-lib.sh|hook_sid_key\(\)|kb-capture-throttle.sh|safe_ulid|safe="\$\(printf .%s. "\$base"'; then
  bad "a hook still uses the lossy session key"
else
  ok "only the documented exceptions keep the lossy form"
fi

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" = 0 ]
