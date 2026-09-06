#!/usr/bin/env bash
# kb-beat.sh — LSC-3 push collection layer: reports one session lifecycle
# EVENT to the kb daemon's live-sessions beat route. Design:
# docs/research/kb-live-sessions-cockpit-2026-08.html §4 (collection,
# harness by harness) + §5 (the wire contract) + §11 (hook-latency /
# beat-volume risk list).
#
# One script, all five harnesses. The harness + the ALREADY-MAPPED
# canonical event name arrive as argv — the mapping from each harness's
# own hook/event vocabulary lives in the CALLER (hooks.json, codex's
# ~/.codex/hooks.json, Kimi's config.toml, opencode's plugin `event`
# handler), not here:
#
#   kb-beat.sh <harness> <event>
#   kb-beat.sh claude turn_end
#
# THE DISCIPLINE (design §5): a beat reports an EVENT, never a STATE.
# event is one of start|prompt|tool|turn_end|blocked|unblocked|end — this
# script must NEVER emit "waiting"/"working"/any derived state. The
# daemon (next phase) derives state from the event stream; blurring that
# split defeats the whole design.
#
# NON-NEGOTIABLE (design §11 risk list):
#   1. Fire-and-forget — the curl runs in a background subshell, fully
#      detached from this process's stdio, so a caller that waits on pipe
#      closure (Claude Code's hook runner, opencode's node child_process)
#      is never blocked on it. This script itself returns almost
#      immediately either way.
#   2. Hard timeout — curl gets --max-time 2 --connect-timeout 1.
#   3. Unconditional exit 0 — no curl, no jq, no token, daemon down,
#      route 404 (it does not exist until the next phase — that is WHY
#      this is safe to ship now), or malformed payload may ever fail a
#      turn.
#   4. Opt-out + no-op-by-default where kb is not set up — KB_BEAT=0 is
#      the kill switch; absent KB_SESSIONS_DIR (kb not configured for
#      this project — the SAME gate every capture hook in this dir
#      already uses) is a silent no-op.
#   5. No stdout/stderr noise — hooks share the operator's terminal.
#
# Env:
#   KB_BEAT=0                    kill switch — exit 0, send nothing.
#   KB_SESSIONS_DIR              required; same "kb is set up here" gate
#                                 kb-capture.sh / kb-capture-throttle.sh
#                                 already use (a beat needs no sessions
#                                 dir of its own — this only signals kb IS
#                                 configured for the project).
#   KB_DAEMON_URL                daemon base URL, same default+override
#                                 convention as kb-wake.sh/kb-recall.sh
#                                 (default http://127.0.0.1:4000).
#   KB_BEAT_PATH                 beat route path (default
#                                 /api/sessions/beat). NOTE: the design
#                                 doc's canonical route is kb-scoped —
#                                 POST /api/kb/{kb}/sessions/beat — but a
#                                 hook has no kb-name env to hand it (only
#                                 KB_SESSIONS_DIR, a filesystem path), and
#                                 the phase brief that commissioned this
#                                 script names the unscoped path. Left
#                                 overridable so the next phase (which
#                                 lands the actual route) can repoint this
#                                 without touching the script.
#   KB_BEAT_CONTENT=0            omit `last_line` (Claude Stop's
#                                 last_assistant_message) even when
#                                 available.
#   KB_BEAT_LEASE_SECS           lease_secs sent with every beat (default
#                                 900 — design §5's own example value).
#   KB_BEAT_DRYRUN=1             print the JSON body to stdout INSTEAD of
#                                 posting; no backgrounding, no curl. The
#                                 test harness's only hook into this
#                                 script (tests/test-beat.sh) and a handy
#                                 manual sanity check.
set -u

# --- kill switch + kb-not-configured no-op — before ANY I/O. ---------------
case "${KB_BEAT:-}" in
  0 | false | FALSE | no | NO) exit 0 ;;
esac
[ -n "${KB_SESSIONS_DIR:-}" ] || exit 0
command -v curl >/dev/null 2>&1 || exit 0
command -v jq >/dev/null 2>&1 || exit 0

harness="${1:-}"
event="${2:-}"
[ -n "$harness" ] && [ -n "$event" ] || exit 0

input="$(cat 2>/dev/null)" || input=""
[ -n "$input" ] || exit 0

# jq_field <filter> <json> — best-effort extraction; never fails the
# caller (a bad filter or malformed JSON just yields empty stdout).
jq_field() {
  printf '%s' "$2" | jq -r "$1" 2>/dev/null
}

# --- session_id — per-harness, reusing each existing capture adapter's ----
# own extraction rather than re-deriving it.
session_id=""
case "$harness" in
  codex)
    # kb-capture-codex.sh's own preference: the rollout's OWN
    # session_meta.payload.id is the canonical id (ground truth, the same
    # #11 preference ladder every harness's capture pipeline follows) —
    # not whatever the hook payload itself might claim. The rollout is
    # named by .transcript_path, exactly as kb-capture-codex.sh reads it.
    tpath="$(jq_field '.transcript_path // empty' "$input")"
    if [ -n "$tpath" ] && [ -f "$tpath" ]; then
      session_id="$(head -1 "$tpath" 2>/dev/null \
        | jq -r 'select(.type == "session_meta") | .payload.id // empty' 2>/dev/null)"
    fi
    # Early-lifecycle codex events (start/prompt) may fire before the
    # rollout carries a session_meta line yet, or the hook payload may
    # simply not include .transcript_path for them — fall back to a bare
    # .session_id if the payload happens to carry one.
    [ -n "$session_id" ] || session_id="$(jq_field '.session_id // empty' "$input")"
    ;;
  *)
    # claude, kimi (README: "stdin payload carries hook_event_name,
    # session_id, cwd, client_type"), opencode (the plugin normalizes its
    # own event shape to this same {session_id, cwd, ...} contract before
    # piping stdin here — see the hooks README's opencode registration
    # note), and grok (best-effort — no live-verified push-hook payload
    # shape exists yet; dormant until a future phase wires an actual
    # registration) all carry .session_id directly.
    session_id="$(jq_field '.session_id // empty' "$input")"
    ;;
esac
[ -n "$session_id" ] || exit 0

cwd="$(jq_field '.cwd // empty' "$input")"
model="$(jq_field '.model // empty' "$input")"

# last_line — Claude Stop ONLY, verbatim .last_assistant_message, capped
# at 240 chars, opt-out via KB_BEAT_CONTENT=0.
last_line=""
if [ "$harness" = "claude" ] && [ "$event" = "turn_end" ] \
   && [ "${KB_BEAT_CONTENT:-}" != "0" ]; then
  last_line="$(jq_field '.last_assistant_message // empty' "$input")"
  last_line="${last_line:0:240}"
fi

# detail.reason — blocked only, whatever the Notification payload gives.
detail_reason=""
if [ "$event" = "blocked" ]; then
  detail_reason="$(jq_field '.message // .matcher // empty' "$input")"
fi

host="$(hostname -s 2>/dev/null || hostname 2>/dev/null || echo unknown)"
pid="${PPID:-0}"
case "$pid" in '' | *[!0-9]*) pid=0 ;; esac
at="$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null)" || at=""
[ -n "$at" ] || exit 0
lease_secs="${KB_BEAT_LEASE_SECS:-900}"
case "$lease_secs" in '' | *[!0-9]*) lease_secs=900 ;; esac

body="$(jq -n \
  --arg session_id "$session_id" \
  --arg harness "$harness" \
  --arg event "$event" \
  --arg at "$at" \
  --arg host "$host" \
  --argjson pid "$pid" \
  --arg cwd "$cwd" \
  --arg model "$model" \
  --argjson lease_secs "$lease_secs" \
  --arg last_line "$last_line" \
  --arg detail_reason "$detail_reason" \
  '{v: 1, session_id: $session_id, harness: $harness, event: $event, at: $at}
   + (if $host != "" then {host: $host} else {} end)
   + (if $pid > 0 then {pid: $pid} else {} end)
   + (if $cwd != "" then {cwd: $cwd} else {} end)
   + (if $model != "" then {model: $model} else {} end)
   + {lease_secs: $lease_secs}
   + (if $last_line != "" then {last_line: $last_line} else {} end)
   + (if $detail_reason != "" then {detail: {reason: $detail_reason}} else {} end)
  ' 2>/dev/null)"
[ -n "$body" ] || exit 0

if [ "${KB_BEAT_DRYRUN:-}" = "1" ]; then
  printf '%s\n' "$body"
  exit 0
fi

daemon="${KB_DAEMON_URL:-http://127.0.0.1:4000}"
route="${KB_BEAT_PATH:-/api/sessions/beat}"
url="${daemon%/}${route}"

token=""
token_file="${XDG_CONFIG_HOME:-$HOME/.config}/kb/token"
[ -r "$token_file" ] && token="$(head -1 "$token_file" 2>/dev/null)"

# Fire-and-forget: fully detached background subshell (stdio redirected
# away from THIS process's pipes, or a caller that waits on pipe closure
# — Claude Code's hook runner, node's child_process — would block on the
# grandchild curl). Hard timeout matches design §11 ("hook latency").
(
  export NO_PROXY="127.0.0.1,localhost${NO_PROXY:+,$NO_PROXY}"
  export no_proxy="127.0.0.1,localhost${no_proxy:+,$no_proxy}"
  unset HTTP_PROXY HTTPS_PROXY http_proxy https_proxy ALL_PROXY all_proxy
  if [ -n "$token" ]; then
    curl -fsS --max-time 2 --connect-timeout 1 \
      -H "Content-Type: application/json" \
      -H "Authorization: Bearer $token" \
      -X POST --data-binary "$body" "$url"
  else
    curl -fsS --max-time 2 --connect-timeout 1 \
      -H "Content-Type: application/json" \
      -X POST --data-binary "$body" "$url"
  fi
) </dev/null >/dev/null 2>&1 &
disown 2>/dev/null || true

exit 0
