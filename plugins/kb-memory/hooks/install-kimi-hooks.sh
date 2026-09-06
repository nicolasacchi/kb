#!/usr/bin/env bash
# install-kimi-hooks.sh — register the kb-memory hooks for Kimi Code.
#
# Kimi Code hooks live in ${KIMI_CODE_HOME:-~/.kimi-code}/config.toml as
# [[hooks]] entries (event / matcher / command / timeout). This installer
# inserts an idempotent, marker-delimited block with the kb entries:
#
#   UserPromptSubmit  kb-wake-kimi.sh           protocol + markers, 1x/session
#   UserPromptSubmit  kb-recall.sh (kimi fmt)   memory recall on every prompt
#   UserPromptSubmit  kb-beat.sh kimi prompt    LSC-3 live-session beat
#   Stop              kb-capture-kimi.sh        transcript -> $KB_SESSIONS_DIR
#   Stop              kb-distill-nudge-kimi.sh  commits-without-remember nudge
#   Stop              kb-beat.sh kimi turn_end  LSC-3 live-session beat
#   SessionEnd        kb-capture-kimi.sh        final capture on exit/archive
#   SessionEnd        kb-beat.sh kimi end       LSC-3 live-session beat (finished)
#   PreCompact        kb-capture-kimi.sh        fresh capture before compact
#
# The three kb-beat.sh entries are the LSC-3 push-collection layer (design:
# docs/research/kb-live-sessions-cockpit-2026-08.html): fire-and-forget,
# hard-timeout, unconditional-exit-0 POSTs to the daemon's live-sessions
# beat route — never load-bearing, never blocking a hook chain. No
# SessionStart entry: kb-beat.sh doesn't need one that isn't already
# proven to fire for kimi (only UserPromptSubmit/Stop/SessionEnd/
# PreCompact are used above), and no kimi Notification-equivalent is
# verified either, so "blocked" stays unwired here (same honest gap
# Claude Code's own Notification wiring calls out as best-effort-only).
#
# Usage:
#   install-kimi-hooks.sh [--sessions-dir DIR] [--config PATH] [--uninstall]
#
# Re-running replaces the previously installed block. --uninstall removes it.
# The config file is backed up next to itself before every mutation.
set -euo pipefail

HOOKS_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
CONFIG="${KIMI_CODE_HOME:-$HOME/.kimi-code}/config.toml"
SESSIONS_DIR="${KB_SESSIONS_DIR:-$HOME/kb/sessions}"
UNINSTALL=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --sessions-dir) SESSIONS_DIR="$2"; shift 2 ;;
    --config)       CONFIG="$2"; shift 2 ;;
    --uninstall)    UNINSTALL=1; shift ;;
    -h|--help)      sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

BEGIN="# >>> kb-memory hooks (kimi) >>>"
END="# <<< kb-memory hooks (kimi) <<<"

[ -f "$CONFIG" ] || { echo "no kimi config at $CONFIG — is Kimi Code installed?" >&2; exit 1; }

backup="$CONFIG.bak-$(date +%Y%m%d%H%M%S)"
cp "$CONFIG" "$backup"

# Strip any previously installed block (idempotent re-install / uninstall).
tmp="$(mktemp)"
awk -v begin="$BEGIN" -v end="$END" '
  $0 == begin { skip = 1; next }
  $0 == end   { skip = 0; next }
  !skip       { print }
' "$CONFIG" > "$tmp"

if [ "$UNINSTALL" -eq 0 ]; then
  for s in kb-wake-kimi.sh kb-recall.sh kb-capture-kimi.sh kb-distill-nudge-kimi.sh kb-beat.sh; do
    [ -x "$HOOKS_DIR/$s" ] || { echo "missing executable: $HOOKS_DIR/$s" >&2; rm -f "$tmp"; exit 1; }
  done
  cat >> "$tmp" <<EOF

$BEGIN
[[hooks]]
event = "UserPromptSubmit"
command = "$HOOKS_DIR/kb-wake-kimi.sh"
timeout = 15

[[hooks]]
event = "UserPromptSubmit"
command = "env KB_HOOK_FMT=kimi $HOOKS_DIR/kb-recall.sh"
timeout = 15

[[hooks]]
event = "UserPromptSubmit"
command = "env KB_SESSIONS_DIR=$SESSIONS_DIR $HOOKS_DIR/kb-beat.sh kimi prompt"
timeout = 5

[[hooks]]
event = "Stop"
command = "env KB_SESSIONS_DIR=$SESSIONS_DIR $HOOKS_DIR/kb-capture-kimi.sh"
timeout = 120

[[hooks]]
event = "Stop"
command = "env KB_SESSIONS_DIR=$SESSIONS_DIR $HOOKS_DIR/kb-distill-nudge-kimi.sh"
timeout = 10

[[hooks]]
event = "Stop"
command = "env KB_SESSIONS_DIR=$SESSIONS_DIR $HOOKS_DIR/kb-beat.sh kimi turn_end"
timeout = 5

[[hooks]]
event = "SessionEnd"
command = "env KB_SESSIONS_DIR=$SESSIONS_DIR $HOOKS_DIR/kb-capture-kimi.sh"
timeout = 120

[[hooks]]
event = "SessionEnd"
command = "env KB_SESSIONS_DIR=$SESSIONS_DIR $HOOKS_DIR/kb-beat.sh kimi end"
timeout = 5

[[hooks]]
event = "PreCompact"
command = "env KB_SESSIONS_DIR=$SESSIONS_DIR $HOOKS_DIR/kb-capture-kimi.sh"
timeout = 120
$END
EOF
fi

mv "$tmp" "$CONFIG"
if [ "$UNINSTALL" -eq 1 ]; then
  echo "kb-memory hooks removed from $CONFIG (backup: $backup)"
else
  echo "kb-memory hooks installed into $CONFIG (backup: $backup)"
  echo "sessions dir: $SESSIONS_DIR"
  echo "takes effect on the NEXT kimi session start."
fi
