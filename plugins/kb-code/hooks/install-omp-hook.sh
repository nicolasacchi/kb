#!/usr/bin/env bash
# install-omp-hook.sh — wire kb-code provenance into Oh My Pi (omp) by
# symlinking kb-code-omp.ts into the omp agent extensions directory, where
# omp's native auto-discovery loads every *.ts as an extension module.
#
# Mirrors plugins/kb-memory/hooks/install-omp-hooks.sh: the symlink (not a
# copy) is deliberate — omp's extension loader resolves the entry's realpath
# before importing and cache-busts on mtime, so edits to this repo propagate
# to the next session with no reinstall.
#
# Idempotent; backs up any pre-existing different file; --uninstall removes
# only what this script installed.
#
# Usage:
#   plugins/kb-code/hooks/install-omp-hook.sh [--agent-dir DIR] [--check]
#   plugins/kb-code/hooks/install-omp-hook.sh --uninstall [--agent-dir DIR]
#
# The extension shells out to its sibling kb-code-why.sh via the resolved
# symlink target's directory (import.meta.dir), so it keeps working if THIS
# REPO moves; re-run after moving to refresh nothing — only the symlink
# source matters, which is inside the same directory tree anyway.
set -eu

AGENT_DIR="${PI_CODING_AGENT_DIR:-${OMP_AGENT_DIR:-$HOME/.omp/agent}}"
SRC="$(cd "$(dirname "$0")" && pwd)/kb-code-omp.ts"
TARGET_NAME="kb-code.ts"

usage() {
  sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --agent-dir) AGENT_DIR="$2"; shift 2 ;;
    --uninstall) UNINSTALL=1; shift ;;
    --check) CHECK=1; shift ;;
    -h|--help) usage 0 ;;
    *) echo "install-omp-hook.sh: unknown option: $1" >&2; usage 1 ;;
  esac
done

EXT_DIR="$AGENT_DIR/extensions"
TARGET="$EXT_DIR/$TARGET_NAME"

is_ours() { # <path> -> 0 when it is our symlink
  [ -L "$1" ] && [ "$(readlink -f "$1")" = "$(readlink -f "$SRC")" ]
}

if [ "${UNINSTALL:-0}" = 1 ]; then
  if [ ! -e "$TARGET" ] && [ ! -L "$TARGET" ]; then
    echo "omp: $TARGET not installed — nothing to uninstall."
    exit 0
  fi
  if is_ours "$TARGET"; then
    rm -f "$TARGET"
    rmdir "$EXT_DIR" 2>/dev/null || true
    echo "omp: removed $TARGET"
    exit 0
  fi
  echo "omp: refusing to remove $TARGET — not installed by this script." >&2
  exit 1
fi

[ -f "$SRC" ] || { echo "install-omp-hook.sh: missing $SRC" >&2; exit 1; }

if is_ours "$TARGET"; then
  echo "omp: already installed — $TARGET -> $(readlink "$TARGET")"
  exit 0
fi

mkdir -p "$EXT_DIR"

if [ -e "$TARGET" ] || [ -L "$TARGET" ]; then
  backup="$TARGET.bak.$(date -u +%Y%m%dT%H%M%SZ)"
  mv "$TARGET" "$backup"
  echo "omp: backed up existing extension to $backup"
fi

ln -s "$SRC" "$TARGET"
echo "omp: installed $TARGET -> $SRC"
echo "omp: loads on the NEXT session start (extensions load at startup)."
echo "omp: kb-code daemon gate: KB_CODE_DAEMON_URL=${KB_CODE_DAEMON_URL:-http://127.0.0.1:4747} (export it in your shell profile if you run kb-code on another port)."
