#!/usr/bin/env bash
# kb-hook-lib.sh — helpers every kb-memory hook shares (v0.44 F6, closing the
# H1 carry-over: five divergent copies of post_distill_ask and per-call caps
# that did not add up against the hooks.json timeouts).
#
# SOURCED, never executed. Every consumer sources it fail-open:
#
#   . "$(dirname "$0")/kb-hook-lib.sh" 2>/dev/null || { <inline no-op stubs>; }
#
# so a standalone copy of one hook (README "Mode 1") that lacks this file
# degrades to unbounded calls / no distill ask instead of breaking the turn.
#
# One deadline model, two knobs:
#   KB_HOOK_BUDGET_SECS  total wall budget for the hook (default 13, which is
#                        below the 15s UserPromptSubmit/SessionStart timeouts
#                        in hooks.json — test-hook-deadlines.sh pins that).
#   run_to <cap> cmd...  runs cmd under min(cap, what is left of the budget);
#                        a call with nothing left is skipped (rc 124 = a miss,
#                        never a kill).
# The clock is milliseconds when the shell can supply them, so a hook that
# starts at x.9s is not credited a whole phantom second.

# Milliseconds since the epoch. EPOCHREALTIME (bash 5) is the cheap path; GNU
# date +%s%N next; whole seconds as the last resort.
hook_now_ms() {
  if [ -n "${EPOCHREALTIME:-}" ]; then
    local s="${EPOCHREALTIME%[.,]*}" f="${EPOCHREALTIME#*[.,]}"
    f="${f}000"
    printf '%s' "$((10#$s * 1000 + 10#${f:0:3}))"
    return 0
  fi
  local n
  n="$(date +%s%N 2>/dev/null)"
  case "$n" in
    '' | *[!0-9]*) printf '%s' "$(($(date +%s 2>/dev/null || echo 0) * 1000))" ;;
    *) printf '%s' "$((n / 1000000))" ;;
  esac
}

# Start the shared deadline. Call once, early.
hook_deadline_init() {
  hook_t0_ms="$(hook_now_ms)"
  hook_budget_ms=$((${KB_HOOK_BUDGET_SECS:-13} * 1000))
}

# Milliseconds left on the shared deadline (never negative).
hook_left_ms() {
  [ -n "${hook_t0_ms:-}" ] || hook_deadline_init
  local left=$((hook_budget_ms - ($(hook_now_ms) - hook_t0_ms)))
  [ "$left" -gt 0 ] || left=0
  printf '%s' "$left"
}

# Fractional-seconds support in timeout(1), probed once.
hook_timeout_frac=""
hook_fmt_secs() { # <ms> -> seconds string timeout(1) accepts
  local ms="$1"
  if [ -z "$hook_timeout_frac" ]; then
    if timeout 0.01 true >/dev/null 2>&1; then hook_timeout_frac=1; else hook_timeout_frac=0; fi
  fi
  if [ "$hook_timeout_frac" = 1 ]; then
    printf '%d.%03d' "$((ms / 1000))" "$((ms % 1000))"
  else
    printf '%d' "$(((ms + 999) / 1000))"
  fi
}

run_to() {
  local cap="$1" left cap_ms
  shift
  left="$(hook_left_ms)"
  [ "$left" -gt 0 ] || return 124
  cap_ms=$((cap * 1000))
  [ "$left" -lt "$cap_ms" ] && cap_ms="$left"
  if command -v timeout >/dev/null 2>&1; then
    timeout "$(hook_fmt_secs "$cap_ms")" "$@"
  else
    "$@"
  fi
}

# Post the "distill this session?" slate ask. Call only after the
# commit-without-remember check. Slate stdout is discarded so it cannot
# corrupt the caller's own output. Never blocks (4s cap, loopback must not
# ride HTTP(S)_PROXY — same reason as kb-wake-kimi.sh).
post_distill_ask() {
  local sid="$1" harness="$2" cwd="${3:-}"
  command -v kb >/dev/null 2>&1 || return 0
  local args=(
    slate ask "Distill session ${sid} (${harness})?"
    --harness "$harness"
    --session-id "$sid"
    --ref "session:${sid}"
  )
  [ -n "$cwd" ] && args+=(--cwd "$cwd")
  (
    # v0.44 X4 — the ask is attributed to the session it is about even when
    # the CLI is not handed the flags (subshell: nothing leaks to the hook).
    export KB_SESSION_ID="$sid" KB_HARNESS="$harness"
    export NO_PROXY="127.0.0.1,localhost${NO_PROXY:+,$NO_PROXY}"
    export no_proxy="127.0.0.1,localhost${no_proxy:+,$no_proxy}"
    unset HTTP_PROXY HTTPS_PROXY http_proxy https_proxy ALL_PROXY all_proxy
    if command -v timeout >/dev/null 2>&1; then
      timeout 4 kb "${args[@]}" >/dev/null 2>&1 || true
    else
      kb "${args[@]}" >/dev/null 2>&1 || true
    fi
  )
}

# Export the session identity every `kb` CLI write resolves first
# (KB_SESSION_ID, then KB_HARNESS — crates/kb-cli/src/session_identity.rs), so
# a shell `kb remember` / `kb slate ...` run by or beside this hook is
# attributed to THIS session instead of falling to the last-writer-wins marker.
#
#   hook_export_identity <session-id> [harness]
#
# * exports into the hook's own environment (children it spawns);
# * on Claude Code's SessionStart, $CLAUDE_ENV_FILE is the one channel that
#   carries an export into the AGENT's later Bash tool calls, so the same two
#   lines are appended there (values %q-quoted; a missing/unwritable file is
#   silently skipped);
# * an unknown harness is never guessed: KB_HARNESS is left as the caller
#   already had it (the CLI then falls back to the session env, then `claude`).
# A blank session id is a no-op. Never fails.
hook_export_identity() {
  local sid="${1:-}" harness="${2:-}"
  [ -n "$sid" ] || return 0
  export KB_SESSION_ID="$sid"
  [ -n "$harness" ] && export KB_HARNESS="$harness"
  if [ -n "${CLAUDE_ENV_FILE:-}" ]; then
    {
      printf 'export KB_SESSION_ID=%q\n' "$sid"
      [ -n "$harness" ] && printf 'export KB_HARNESS=%q\n' "$harness"
    } >>"$CLAUDE_ENV_FILE" 2>/dev/null || true
  fi
  return 0
}

# --- capture spool (v0.44 X6) ------------------------------------------------
# When `kb sessions capture` fails, a capture hook must NOT hand-write the raw
# transcript into the corpus (that path skipped the secrets scrub). It parks the
# raw transcript in a private spool OUTSIDE every corpus instead; the next
# successful capture (or `kb sessions capture --replay-spool`) pushes it through
# the normal scrubbed path and deletes it. The dir resolution mirrors
# `sessions_capture::spool_dir` in kb-cli exactly:
#   $KB_CAPTURE_SPOOL > $KB_CACHE_DIR/capture-spool >
#   $XDG_CACHE_HOME/kb/capture-spool > $HOME/.cache/kb/capture-spool

hook_spool_dir() {
  if [ -n "${KB_CAPTURE_SPOOL:-}" ]; then
    printf '%s' "$KB_CAPTURE_SPOOL"
  elif [ -n "${KB_CACHE_DIR:-}" ]; then
    printf '%s/capture-spool' "$KB_CACHE_DIR"
  elif [ -n "${XDG_CACHE_HOME:-}" ]; then
    printf '%s/kb/capture-spool' "$XDG_CACHE_HOME"
  elif [ -n "${HOME:-}" ]; then
    printf '%s/.cache/kb/capture-spool' "$HOME"
  else
    return 1
  fi
}

# hook_spool_key <raw-session-id>
# The spool file stem. A plain id ([A-Za-z0-9-], <=80 chars - every UUID) keeps
# its own name; anything else gets a sanitised prefix PLUS a hash of the FULL
# raw id, so two distinct ids ("a_b" vs "a-b", or two ids sharing an 80-char
# prefix) can never share one spool slot and overwrite each other.
hook_spool_key() {
  local raw="$1" safe h
  safe="$(printf '%s' "$raw" | tr -c 'a-zA-Z0-9' '-')"
  if [ -n "$raw" ] && [ "$safe" = "$raw" ] && [ "${#raw}" -le 80 ]; then
    printf '%s' "$raw"
    return 0
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    h="$(printf '%s' "$raw" | sha256sum | cut -c1-16)"
  elif command -v shasum >/dev/null 2>&1; then
    h="$(printf '%s' "$raw" | shasum -a 256 | cut -c1-16)"
  else
    h="$(printf '%s' "$raw" | cksum | tr ' ' '-')"
  fi
  safe="$(printf '%s' "$safe" | cut -c1-48)"
  printf '%s-%s' "${safe:-session}" "$h"
}

# hook_sid_key <raw-session-id>
# THE per-session file-name key for every hook marker, throttle file and
# adapter capture name. Identical to the spool key: a plain id (every UUID)
# is unchanged, any other id carries a hash of the FULL raw id, so the lossy
# `tr -c ... | cut -c1-80` form (which maps "a_b" and "a-b", or two ids that
# share an 80-char prefix, to ONE name) is no longer used for per-session state.
hook_sid_key() { hook_spool_key "$1"; }

# hook_sid_key_lossy <raw-session-id>
# The pre-v0.44 lossy key. Kept ONLY so a cleanup can recognise legacy files.
hook_sid_key_lossy() { printf '%s' "$1" | tr -c 'a-zA-Z0-9' '-' | cut -c1-80; }

# hook_marker_seen <path-prefix> <raw-session-id>
# True when a once-per-session marker exists under the unified key OR (upgrade
# compatibility) under the pre-v0.45 lossy name, so a session nudged/waked
# before the upgrade is not nudged a second time. Markers are empty files, so
# the legacy name cannot be verified against an embedded id: a colliding id
# (e.g. "a_b" vs "a-b") may inherit the other's legacy marker, which at worst
# suppresses one nudge - the failure direction the marker already had.
# New markers are always written under the unified key.
hook_marker_seen() {
  local prefix="$1" raw="$2" key legacy
  key="$(hook_sid_key "$raw")"
  [ -f "${prefix}${key}" ] && return 0
  legacy="$(hook_sid_key_lossy "$raw")"
  [ "$legacy" != "$key" ] && [ -f "${prefix}${legacy}" ] && return 0
  return 1
}

# hook_spool_put <transcript> <raw-session-id> [cwd] [stamp] [harness]
# `stamp` (compact UTC, the session's true start time) and `harness` ride the
# .meta so a replayed capture keeps its start-time filename (v0.45 N4); the
# replay ignores keys it does not know. Dir 0700, files 0600, one item per
# session (latest snapshot wins, as the corpus file does). Refuses a transcript over the 48MiB capture cap. Returns
# non-zero when nothing was spooled; never writes anywhere but the spool.
hook_spool_put() {
  local tpath="$1" raw_sid="$2" cwd="${3:-}" stamp="${4:-}" harness="${5:-}" dir key tmp size
  dir="$(hook_spool_dir)" || return 1
  size="$(stat -c %s "$tpath" 2>/dev/null || wc -c <"$tpath" 2>/dev/null)"
  if [ -n "$size" ] && [ "$size" -gt 50331648 ]; then
    echo "kb-hook-lib: not spooling oversized transcript ($size bytes > 48MiB cap): $tpath" >&2
    return 1
  fi
  key="$(hook_spool_key "$raw_sid")"
  (
    umask 077
    mkdir -p "$dir" && chmod 700 "$dir" || exit 1
    tmp="$dir/.$key.jsonl.tmp.$$"
    cp "$tpath" "$tmp" || { rm -f "$tmp"; exit 1; }
    {
      printf 'session_id=%s\n' "$raw_sid"
      [ -n "$cwd" ] && printf 'cwd=%s\n' "$cwd"
      [ -n "$stamp" ] && printf 'stamp=%s\n' "$stamp"
      [ -n "$harness" ] && printf 'harness=%s\n' "$harness"
      true
    } >"$dir/$key.meta" || { rm -f "$tmp"; exit 1; }
    mv -f "$tmp" "$dir/$key.jsonl"
  )
}

# hook_spool_drop <raw-session-id>
# Remove this session's spool item (same key as hook_spool_put). Called after a
# successful capture of the session: the spooled snapshot is older than what
# just landed, and replaying it would overwrite the newer corpus file.
hook_spool_drop() {
  local dir key legacy
  dir="$(hook_spool_dir)" || return 0
  key="$(hook_spool_key "$1")"
  rm -f "$dir/$key.jsonl" "$dir/$key.meta"
  # Parked subagent sidecars of this session (hook_spool_put_sidecars).
  case "$1" in "" | *[!A-Za-z0-9_-]*) ;; *) rm -rf "${dir:?}/$1" ;; esac
  # A spool item written before the hashed key existed sits under the lossy
  # name. For a non-plain id that name differs from `key`; drop it too, but
  # ONLY when its .meta records exactly this raw id (a colliding id's item
  # must survive).
  legacy="$(hook_sid_key_lossy "$1")"
  if [ "$legacy" != "$key" ] && [ -f "$dir/$legacy.meta" ] \
    && grep -qxF "session_id=$1" "$dir/$legacy.meta" 2>/dev/null; then
    rm -f "$dir/$legacy.jsonl" "$dir/$legacy.meta"
  fi
}

# hook_spool_put_sidecars <raw-session-id> <subagents-dir>
# Park an adapter's translated subagent sidecars (agent-*.jsonl) next to its
# spooled main transcript, at <spool>/<raw-session-id>/subagents/ - exactly
# where `kb sessions capture` looks for them (transcript.parent()/<raw sid>/
# subagents), so a replay folds them into the capture with no further wiring.
# Only a plain id ([A-Za-z0-9_-]) is accepted as a directory name; anything
# else is refused (the main transcript still replays, sidecars are skipped).
# `kb sessions capture --replay-spool` removes the dir after a successful
# replay; hook_spool_drop removes it when a live capture lands first.
hook_spool_put_sidecars() {
  local raw="$1" src="$2" dir
  case "$raw" in "" | *[!A-Za-z0-9_-]*) return 1 ;; esac
  [ -d "$src" ] || return 1
  dir="$(hook_spool_dir)" || return 1
  (
    umask 077
    mkdir -p "$dir" && chmod 700 "$dir" || exit 1
    rm -rf "${dir:?}/$raw"
    mkdir -p "$dir/$raw/subagents" || exit 1
    cp "$src"/*.jsonl "$dir/$raw/subagents/" 2>/dev/null || true
  )
}

# hook_kb_in_path
# Put `kb` on PATH in THIS shell (a harness hook often runs with a minimal
# PATH): the usual install dirs and KB_BIN_DIR are probed, as hook_adapter_land
# does inside its own call. rc 1 = not found. (v0.46 SEG-PR2: the segmented
# omp capture probes `kb sessions segment-plan` in the main shell, because
# hook_adapter_land runs in a subshell and its PATH change does not survive.)
hook_kb_in_path() {
  command -v kb >/dev/null 2>&1 && return 0
  local d
  for d in "${KB_BIN_DIR:-}" "${HOME:-}/.local/bin" "${HOME:-}/.cargo/bin" /usr/local/bin /opt/homebrew/bin; do
    if [ -n "$d" ] && [ -x "$d/kb" ]; then
      PATH="$d:$PATH"
      return 0
    fi
  done
  return 1
}

# hook_spool_count_group <raw-session-id>
# How many spool items belong to ONE segmented session: the bare raw id plus
# every `<raw id>-p<NN>` continuation part (matched on the `session_id=` first
# line of each item's .meta, never on the file name - a non-plain id's key
# carries a hash of the full part id, so parts share no name prefix). Prints
# the count. v0.46 SEG-PR2: the adapter caps the parked parts of one session.
hook_spool_count_group() {
  local dir
  dir="$(hook_spool_dir)" || { printf '0'; return 0; }
  # shellcheck disable=SC2231
  awk -v raw="$1" '
    FNR == 1 {
      want = "session_id=" raw
      if ($0 == want) c++
      else if (index($0, want "-p") == 1 && substr($0, length(want) + 3) ~ /^[0-9]+$/) c++
    }
    END { print c + 0 }
  ' "$dir"/*.meta 2>/dev/null || printf '0'
}

# hook_short_hash <string> - first 8 hex of sha256 (cksum fallback).
hook_short_hash() {
  if command -v sha256sum >/dev/null 2>&1; then
    printf '%s' "$1" | sha256sum | cut -c1-8
  elif command -v shasum >/dev/null 2>&1; then
    printf '%s' "$1" | shasum -a 256 | cut -c1-8
  else
    printf '%s' "$1" | cksum | tr ' ' '-'
  fi
}

# hook_agent_safe_name <agent-id> <agents-src-dir>
# The readable file-name stem for a subagent sidecar (omp: agent-<stem>.jsonl).
# NOT a session key: it names an agent, so it keeps the readable lossy form.
# Two different agent ids can map to the same stem ("Web UI & Tests" vs
# "Web-UI---Tests"). The choice depends ONLY on the SET of ids found as
# `<agents-src-dir>/*.jsonl`, never on discovery order: an id that already is
# its own stem keeps the plain name; any other id whose stem is shared with a
# different id gets `-<8 hex of sha256(id)>` appended; an id with a unique stem
# keeps the plain stem. Empty stem => returns 1 (caller skips the agent).
hook_agent_stem() { printf '%s' "$1" | tr -c 'a-zA-Z0-9' '-' | cut -c1-80; }
hook_agent_safe_name() {
  local base="$1" dir="$2" stem f other
  stem="$(hook_agent_stem "$base")"
  [ -n "$stem" ] || return 1
  # Already its own stem: nothing else can claim the plain name from it.
  [ "$stem" = "$base" ] && { printf '%s' "$stem"; return 0; }
  for f in "$dir"/*.jsonl; do
    [ -f "$f" ] || continue
    other="$(basename "$f" .jsonl)"
    [ "$other" = "$base" ] && continue
    if [ "$(hook_agent_stem "$other")" = "$stem" ]; then
      printf '%s-%s' "$stem" "$(hook_short_hash "$base")"
      return 0
    fi
  done
  printf '%s' "$stem"
}

# True when the spool holds at least one item.
hook_spool_pending() {
  local dir f
  dir="$(hook_spool_dir)" || return 1
  for f in "$dir"/*.jsonl; do
    [ -f "$f" ] && return 0
  done
  return 1
}

# hook_adapter_land <raw-session-id> <translated-jsonl> <cwd> <stamp> <harness>
# v0.45 N4 - the ONE landing path for the codex/opencode/kimi/omp/grok capture
# adapters. They translate their harness-native transcript into Claude-shaped
# JSONL (first record = `adapter-meta`, whose `harness` is the enrich ladder's
# rung 1, so the harness survives the Rust-written envelope) and hand the
# result here. It is pushed through `kb sessions capture` (envelope + secrets
# scrub, never a bash-written HTML); on success this session's older spool item
# is dropped and any other pending items are replayed. On failure, or with no
# `kb`, the UNSCRUBBED translation is parked in the private spool (0700/0600,
# outside every corpus) for the next success / `--replay-spool`. NOTHING raw
# is ever written to the corpus. The replay covers EVERY session's items while
# the caller holds only its own session's lock, so the Rust writer (not this
# lock) refuses to publish a spooled snapshot over a fresher capture. Returns 0 = landed in the corpus, 1 = spooled,
# 2 = neither (stderr says why). Callers that only run inside a hook ignore it:
# a hook never fails a turn.
# Needs KB_SESSIONS_DIR; cwd/stamp/harness may be empty.
hook_adapter_land() {
  local sid="$1" tj="$2" cwd="${3:-}" stamp="${4:-}" harness="${5:-}" ccwd=""
  [ -d "$cwd" ] && ccwd="$cwd"
  # A harness hook often runs with a minimal PATH: probe the usual install
  # dirs (and KB_BIN_DIR) before concluding `kb` is absent.
  if ! command -v kb >/dev/null 2>&1; then
    local d
    for d in "${KB_BIN_DIR:-}" "${HOME:-}/.local/bin" "${HOME:-}/.cargo/bin" /usr/local/bin /opt/homebrew/bin; do
      if [ -n "$d" ] && [ -x "$d/kb" ]; then
        PATH="$d:$PATH"
        break
      fi
    done
  fi
  if command -v kb >/dev/null 2>&1; then
    # A `kb` older than the plugin has no `--stamp`: retry once without it
    # (the capture then gets a now-stamped name) before giving up to the spool.
    local attempt extra=() base=(--transcript "$tj" --session-id "$sid")
    [ -n "$ccwd" ] && base+=(--cwd "$ccwd")
    for attempt in stamped plain; do
      extra=()
      if [ "$attempt" = stamped ]; then
        [ -n "$stamp" ] || continue
        extra=(--stamp "$stamp")
      fi
      if run_to 20 kb sessions capture "${base[@]}" ${extra[@]+"${extra[@]}"} \
        --out "$KB_SESSIONS_DIR" >/dev/null 2>&1; then
        # The spooled snapshot of THIS session is now older than what landed.
        hook_spool_drop "$sid"
        if hook_spool_pending; then
          run_to 10 kb sessions capture --replay-spool --out "$KB_SESSIONS_DIR" >/dev/null 2>&1 || true
        fi
        return 0
      fi
    done
  fi
  if hook_spool_put "$tj" "$sid" "$cwd" "$stamp" "$harness"; then
    echo "kb-hook-lib: ${harness:-adapter} capture failed - spooled session $sid for replay" >&2
    return 1
  fi
  echo "kb-hook-lib: ${harness:-adapter} capture failed and the transcript could not be spooled (session $sid)" >&2
  return 2
}

# --- capture locks (v0.45 OC) -----------------------------------------------
# Per-session cross-process exclusion for a capture adapter (kb-capture-omp.sh).
# A growing session is captured from several triggers (every turn end, a
# compaction, shutdown) and from several omp processes at once; the Rust writer
# publishes through ONE fixed `<out>.tmp` name, so two concurrent captures of a
# session interleave and a stale one can rename after a fresher one. The lock
# therefore wraps the whole convert + land sequence, keyed on the canonical
# path of the session file (NOT on a session id the caller supplied).
#   $KB_CAPTURE_LOCKS > $KB_CACHE_DIR/capture-locks >
#   $XDG_CACHE_HOME/kb/capture-locks > $HOME/.cache/kb/capture-locks

hook_capture_lock_dir() {
  if [ -n "${KB_CAPTURE_LOCKS:-}" ]; then
    printf '%s' "$KB_CAPTURE_LOCKS"
  elif [ -n "${KB_CACHE_DIR:-}" ]; then
    printf '%s/capture-locks' "$KB_CACHE_DIR"
  elif [ -n "${XDG_CACHE_HOME:-}" ]; then
    printf '%s/kb/capture-locks' "$XDG_CACHE_HOME"
  elif [ -n "${HOME:-}" ]; then
    printf '%s/.cache/kb/capture-locks' "$HOME"
  else
    return 1
  fi
}

# hook_capture_key <path> - 24 hex of sha256(realpath). Fails (rc 1) when the
# path cannot be canonicalised or hashed: the caller then runs unlocked.
hook_capture_key() {
  local rp h
  rp="$(realpath -- "$1" 2>/dev/null || readlink -f -- "$1" 2>/dev/null)" || return 1
  [ -n "$rp" ] || return 1
  if command -v sha256sum >/dev/null 2>&1; then
    h="$(printf '%s' "$rp" | sha256sum | cut -c1-24)"
  elif command -v shasum >/dev/null 2>&1; then
    h="$(printf '%s' "$rp" | shasum -a 256 | cut -c1-24)"
  else
    return 1
  fi
  [ -n "$h" ] || return 1
  printf '%s' "$h"
}
