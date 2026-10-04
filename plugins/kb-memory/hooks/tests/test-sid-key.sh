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

echo "== golden table (identical rows pinned in Rust: import.rs sanitize_sid_matches_hook_spool_key_golden_table) =="
check_key() {
  local raw="$1" want="$2" name="$3" got
  got="$(hook_sid_key "$raw")"
  [ "$got" = "$want" ] && ok "key: $name" || bad "key: $name (got '$got', want '$want')"
}
check_key "$UUID" "$UUID" "uuid is its own key"
check_key 'a_b' 'a-b-648fa9b31bc7ff7e' "a_b"
check_key 'a-b' 'a-b' "a-b"
check_key 'ses_01HXYZ' 'ses-01HXYZ-8413c6b038ea242d' "opencode-style ses_ id"
check_key 'sésión' 's--si--n-857c877373c39c8b' "non-ASCII id (bytewise tr)"
check_key '' 'session-e3b0c44298fc1c14' "empty id"
A81="$(printf 'a%.0s' $(seq 1 81))"
check_key "$A81" 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-8c48280d57fb88f1' "81-char id"
PP="$(printf 'p%.0s' $(seq 1 80))"
check_key "${PP}X" 'pppppppppppppppppppppppppppppppppppppppppppppppp-f697b5af07a87313' "80-char prefix + X"
check_key "${PP}Y" 'pppppppppppppppppppppppppppppppppppppppppppppppp-ffe6644da0d12e79' "80-char prefix + Y"
check_key "$(printf 'a%.0s' $(seq 1 80))" "$(printf 'a%.0s' $(seq 1 80))" "exactly 80 plain chars stay plain"

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

echo "== no_capture_adapter_or_throttle_uses_lossy_key =="
# Scope: the five capture adapters, kb-capture.sh and the capture throttle. The
# marker-file stubs (kb-beat-throttle, kb-distill-nudge*, kb-wake-kimi) still
# carry a local lossy hook_sid_key; they only name marker files and are NOT
# covered by this test. The one other exemption is kb-capture-omp.sh's sidecar
# AGENT file name (`"$base"`): it names a subagent, not a session, and its
# readable form is the visible agent_id (see the PR's deferred list).
if grep -nE "tr -c 'a-zA-Z0-9' '-' \| cut -c1-80" "$HOOKS_DIR"/kb-capture*.sh \
  | grep -vE 'hook_sid_key_lossy\(\)|safe_ulid|"\$base"'; then
  bad "a hook still uses the lossy session key"
else
  ok "only the documented exceptions keep the lossy form"
fi
# Adapters and the throttle must not carry a local hook_sid_key stub at all
# (the throttle's standalone fallback is the HASHED algorithm, checked below).
if grep -nE 'hook_sid_key\(\)' "$HOOKS_DIR"/kb-capture-codex.sh "$HOOKS_DIR"/kb-capture-opencode.sh \
  "$HOOKS_DIR"/kb-capture-kimi.sh "$HOOKS_DIR"/kb-capture-omp.sh "$HOOKS_DIR"/kb-capture-grok.sh; then
  bad "an adapter still defines a local (lossy) hook_sid_key stub"
else
  ok "no adapter defines a local hook_sid_key stub"
fi
if grep -nE "tr -c 'a-zA-Z0-9' '-' \| cut -c1-80" "$HOOKS_DIR/kb-capture-throttle.sh" \
  | grep -vE 'hook_sid_key_lossy\(\)'; then
  bad "the throttle still derives its key with the lossy pipeline"
else
  ok "the throttle's only lossy form is the migration-recognition helper"
fi
echo "== the throttle's standalone copy of the key equals the lib's =="
for raw in "$UUID" 'a_b' 'a-b' 'ses_01HXYZ' 'sésión' "$A81"; do
  inline="$(bash -c '
    src="$(sed -n "/^  hook_sid_key() {/,/^  }/p" "$1")"
    eval "$(printf "%s" "$src" | sed "s/^  //")"
    hook_sid_key "$2"' _ "$HOOKS_DIR/kb-capture-throttle.sh" "$raw")"
  [ "$inline" = "$(hook_sid_key "$raw")" ] && ok "inline throttle key == lib key for '${raw:0:12}'" || bad "inline throttle key diverges for '${raw:0:12}' ($inline)"
done

echo "== hook_spool_put_with_empty_cwd_succeeds_and_writes_meta_without_cwd =="
export KB_CAPTURE_SPOOL="$TMPROOT/spool-nocwd"
printf '{"sessionId":"x"}\n' >"$TMPROOT/nocwd.jsonl"
if hook_spool_put "$TMPROOT/nocwd.jsonl" 'ses_nocwd' '' '20260101T000000Z' 'codex'; then
  ok "spool put with an empty cwd returns 0"
else
  bad "spool put with an empty cwd returns 0"
fi
nk="$(hook_spool_key 'ses_nocwd')"
[ -s "$KB_CAPTURE_SPOOL/$nk.jsonl" ] && ok "empty-cwd item is spooled" || bad "empty-cwd item is spooled"
if grep -q '^session_id=ses_nocwd$' "$KB_CAPTURE_SPOOL/$nk.meta" \
  && grep -q '^stamp=20260101T000000Z$' "$KB_CAPTURE_SPOOL/$nk.meta" \
  && grep -q '^harness=codex$' "$KB_CAPTURE_SPOOL/$nk.meta" \
  && ! grep -q '^cwd=' "$KB_CAPTURE_SPOOL/$nk.meta"; then
  ok "meta carries id/stamp/harness and no cwd line"
else
  bad "meta carries id/stamp/harness and no cwd line"
fi

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" = 0 ]
