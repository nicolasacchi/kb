#!/usr/bin/env bash
# kb-capture-throttle — LF-3a / D2: a min-interval gate in front of
# kb-capture.sh, registered under PostToolUse (and PreCompact) so a
# long-running session gets a FRESH capture every few minutes instead of
# only at Stop — the universal floor that makes the staleness badge (R9b)
# and the M3/Tier-0 presence chip honest at minutes-granularity, in prod,
# through the normal scrubbed capture pipeline (memo R9/LF-1 Tier 0).
#
# Opt-in, DEFAULT OFF (D2, week-1 caution): gated on KB_CAPTURE_LIVE=1 —
# hooks.json ALWAYS registers this entry (mirrors kb-capture.sh's own
# KB_SESSIONS_DIR gate: the script decides whether to act, not the hook
# registration), so enabling live capture is a settings/env flip, never a
# code or hooks.json change.
#
# PostToolUse and Stop carry the SAME payload shape (session_id,
# transcript_path, cwd on stdin) — this script reads it exactly like
# kb-capture.sh does. PreCompact fires once, right before Claude Code
# rewrites the transcript for compaction: it is the ONE moment a
# pre-compaction tail would otherwise be lost forever, so it rides the
# SAME gate + interval logic (a fresh capture right before compaction is
# always worth taking, same cost model as any other throttled fire).
#
# kb-capture.sh itself is left COMPLETELY UNTOUCHED: this is a thin gate
# in FRONT of it, using the identical envelope + one-file-per-sid reuse
# contract (#11 multi-capture) — every read (list/why/recollect/…) already
# newest-capture-scopes, so a mid-run capture is invisible to any existing
# consumer except by making it MORE current.
set -u

case "${KB_CAPTURE_LIVE:-}" in
  1 | true | TRUE | yes | YES) ;;
  *) exit 0 ;;
esac

[ -n "${KB_SESSIONS_DIR:-}" ] || exit 0
command -v jq >/dev/null 2>&1 || exit 0

MIN_INTERVAL="${KB_CAPTURE_MIN_INTERVAL_SECS:-240}"

HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CAPTURE_SH="$HOOK_DIR/kb-capture.sh"
[ -x "$CAPTURE_SH" ] || CAPTURE_SH="$HOOK_DIR/kb-capture.sh" # still exec; bash needs no +x

input="$(cat)"
raw_sid="$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null)"

# No session id at all — nothing to throttle against; let kb-capture.sh
# make its own (identical) determination from the transcript.
if [ -z "$raw_sid" ]; then
  printf '%s' "$input" | "$CAPTURE_SH"
  exit 0
fi

# Resolve the session's existing capture. The file-name key is the ONE shared
# `hook_sid_key` (v0.45 N4): `kb sessions capture` (Rust `sanitize_sid`) names
# its file with the identical algorithm, pinned by a golden table on both
# sides - the interval check must compare against the exact file the capture
# would rewrite. The standalone copy (no lib beside this script) carries an
# inline copy of the same function.
. "$HOOK_DIR/kb-hook-lib.sh" 2>/dev/null || {
  hook_sid_key() {
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
  hook_sid_key_lossy() { printf '%s' "$1" | tr -c 'a-zA-Z0-9' '-' | cut -c1-80; }
}

sid="$(hook_sid_key "$raw_sid")"
raw_lit="$(printf '%s' "$raw_sid" | jq -Rs . 2>/dev/null)"

# The sessionId JSON literal embedded in a capture's head ('' when none).
file_sid_literal() {
  head -c 262144 "$1" 2>/dev/null \
    | grep -o -m1 '"sessionId": *"\([^"\\]\|\\.\)*"' \
    | head -1 | sed 's/^"sessionId": *//'
}

out=""
# Current name: `session-<16-char ts>-<key>.html`, take the newest whose
# embedded id (when it has one) is this session's - a pre-v0.45 lossy name was
# shared by distinct ids, so a name match alone must not adopt a stranger's file.
for f in "$KB_SESSIONS_DIR"/session-????????T??????Z-"$sid.html"; do
  [ -f "$f" ] || continue
  lit="$(file_sid_literal "$f")"
  if [ -z "$lit" ] || [ "$lit" = "$raw_lit" ]; then out="$f"; fi
done

# MIGRATION: a capture written under the pre-v0.45 lossy name still throttles
# its own session, but ONLY when the file's embedded id equals this raw id.
if [ -z "$out" ]; then
  lossy="$(hook_sid_key_lossy "$raw_sid")"
  if [ "$lossy" != "$sid" ]; then
    for f in "$KB_SESSIONS_DIR"/session-????????T??????Z-"$lossy.html" \
             "$KB_SESSIONS_DIR"/session-????????T??????Z-"$lossy-.html"; do
      [ -f "$f" ] || continue
      [ "$(file_sid_literal "$f")" = "$raw_lit" ] && out="$f"
    done
  fi
fi

if [ -z "$out" ]; then
  # First capture for this session - nothing to throttle against, fire.
  printf '%s' "$input" | "$CAPTURE_SH"
  exit 0
fi

now="$(date -u +%s)"
mtime="$(stat -c %Y "$out" 2>/dev/null || stat -f %m "$out" 2>/dev/null || echo 0)"
age=$((now - mtime))

if [ "$age" -ge "$MIN_INTERVAL" ]; then
  printf '%s' "$input" | "$CAPTURE_SH"
fi
# else: too soon since the last capture — skip silently, exit 0.
exit 0
