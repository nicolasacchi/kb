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
# v0.46 SEG-A - segmented-capture part ids (`<raw id>-pNN`).
check_key "${UUID}-p02" "${UUID}-p02" "a uuid part id is its own key"
check_key 'ses_01HXYZ-p02' 'ses-01HXYZ-p02-7006ddf22fd96a3e' "opencode-style part id is hashed"
R77="$(printf 'r%.0s' $(seq 1 77))"
check_key "${R77}-p02" 'rrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrr-11deaa9d76484e97' "81-byte part id is hashed"

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

echo "== beat heartbeat: a pre-upgrade lossy-named marker still throttles (no extra beat) =="
LB="$TMPROOT/legacy-beat"; mkdir -p "$LB/cache/kb" "$LB/bin"
cat >"$LB/bin/kb" <<'SH'
#!/usr/bin/env bash
exit 1
SH
chmod +x "$LB/bin/kb"
cat >"$LB/fake-beat.sh" <<'SH'
#!/usr/bin/env bash
echo beat >>"$BEAT_LOG"
SH
chmod +x "$LB/fake-beat.sh"
mkdir -p "$LB/hooks"; cp "$HOOKS_DIR/kb-beat-throttle.sh" "$HOOKS_DIR/kb-hook-lib.sh" "$LB/hooks/"
cp "$LB/fake-beat.sh" "$LB/hooks/kb-beat.sh"
: >"$LB/cache/kb/beat-heartbeat-$(hook_sid_key_lossy 'ses_old.1')"
: >"$LB/log"
printf '{"session_id":"ses_old.1"}' | BEAT_LOG="$LB/log" XDG_CACHE_HOME="$LB/cache" KB_SESSIONS_DIR="$TMPROOT/sessions" \
  KB_BEAT_HEARTBEAT_MIN_INTERVAL_SECS=3600 bash "$LB/hooks/kb-beat-throttle.sh" claude >/dev/null 2>&1
[ ! -s "$LB/log" ] && ok "legacy-named heartbeat marker throttles the first post-upgrade tool call" || bad "legacy heartbeat marker ignored (beat fired)"

echo "== hook_agent_safe_name is independent of discovery order =="
AD="$TMPROOT/agents"; mkdir -p "$AD"
for id in 'Web UI & Tests' 'Web-UI---Tests' 'Web_UI___Tests' 'solo agent'; do : >"$AD/$id.jsonl"; done
names_a="$(for id in 'Web UI & Tests' 'Web-UI---Tests' 'Web_UI___Tests' 'solo agent'; do printf '%s=%s\n' "$id" "$(hook_agent_safe_name "$id" "$AD")"; done | sort)"
names_b="$(for id in 'solo agent' 'Web_UI___Tests' 'Web-UI---Tests' 'Web UI & Tests'; do printf '%s=%s\n' "$id" "$(hook_agent_safe_name "$id" "$AD")"; done | sort)"
[ "$names_a" = "$names_b" ] && ok "agent names identical in either call order" || bad "agent names depend on call order"
[ "$(hook_agent_safe_name 'Web-UI---Tests' "$AD")" = 'Web-UI---Tests' ] && ok "the id that is already safe keeps the plain name" || bad "self-safe id lost its plain name"
case "$(hook_agent_safe_name 'Web UI & Tests' "$AD")" in Web-UI---Tests-????????) ok "a colliding lossy id gets the hash suffix" ;; *) bad "colliding lossy id not hashed" ;; esac
[ "$(hook_agent_safe_name 'solo agent' "$AD")" = 'solo-agent' ] && ok "a lossy id with a unique stem keeps the plain stem" || bad "unique lossy stem was hashed"
# a SECOND sidecar appearing later must not rename an already-named agent that is self-safe
: >"$AD/Web-UI-.jsonl"
[ "$(hook_agent_safe_name 'Web-UI---Tests' "$AD")" = 'Web-UI---Tests' ] && ok "adding another agent does not rename the self-safe one" || bad "self-safe name changed"

echo "== no_hook_uses_lossy_key_in_any_adapter_or_throttle =="
# Scope: EVERY hook script. The lossy `tr -c ... | cut -c1-80` pipeline may
# appear only in the migration-recognition helper hook_sid_key_lossy() (and
# the lib's own hook_marker_seen/drop callers use that helper by name). No
# per-script exemptions: a local hook_sid_key stub, a marker name, a sidecar
# name - a lossy form anywhere else fails. The single named exception is the
# grok report lane's `safe_ulid`: a job id is a ULID (Crockford base32, already
# [A-Z0-9]), so the map is the identity and cannot collide.
# The pattern is the two halves of the lossy form separately (`tr -c[s]` over
# the a-zA-Z0-9 class, any quoting, and any `cut -c1-80`), so a reformatted or
# split pipeline is still caught. Allowed: the three named helpers/lines above
# and the `safe=` stem line that FEEDS the hashed key (hook_spool_key and the
# throttle's standalone copy, which append a hash of the full id).
if grep -nE "(tr +-cs? +['\"]?a-zA-Z0-9|cut +-c1-80)" "$HOOKS_DIR"/*.sh \
  | grep -vE 'hook_sid_key_lossy\(\)|hook_agent_stem\(\)|safe_ulid=|^[^:]+:[0-9]+:[[:space:]]*safe="\$\(printf .%s. "\$raw" \| tr -c .a-zA-Z0-9. .-.\)"|^[^:]+:[0-9]+:[[:space:]]*#'; then
  bad "a hook still uses the lossy session key"
else
  ok "the only lossy form is the migration-recognition helper"
fi
if grep -nE 'hook_sid_key\(\)' "$HOOKS_DIR"/*.sh | grep -vE 'kb-hook-lib.sh:|kb-capture-throttle.sh:'; then
  bad "a hook script defines its own hook_sid_key (must source the lib)"
else
  ok "hook_sid_key is defined only in kb-hook-lib.sh and the throttle's hashed standalone copy"
fi
echo "== marker upgrade compatibility: an old lossy-named marker still suppresses =="
mdir="$TMPROOT/markers"; mkdir -p "$mdir"
: >"$mdir/distill-nudged-$(hook_sid_key_lossy 'ses_old.1')"
hook_marker_seen "$mdir/distill-nudged-" 'ses_old.1' && ok "legacy marker is honoured (no double fire after upgrade)" || bad "legacy marker ignored"
hook_marker_seen "$mdir/distill-nudged-" 'ses_new.2' && bad "unrelated id wrongly suppressed" || ok "an unrelated id is not suppressed"
: >"$mdir/distill-nudged-$(hook_sid_key 'ses_new.2')"
hook_marker_seen "$mdir/distill-nudged-" 'ses_new.2' && ok "new-key marker is honoured" || bad "new-key marker ignored"
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
