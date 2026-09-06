#!/usr/bin/env bash
# install-omp-hook.sh — wire kb-code's omp extensions into Oh My Pi (omp)
# by symlinking them into the omp agent extensions directory, where omp's
# native auto-discovery loads every *.ts as an extension module.
#
# V71-X1: TWO independent extensions now install here — kb-code-omp.ts
# (provenance, delegates to kb-code-why.sh) and kb-code-annotations-omp.ts
# (operator flags, delegates to kb-code-annotations.sh; recon
# `cli-agent-surface.md` open question 9). Mirrors
# plugins/kb-memory/hooks/install-omp-hooks.sh's own multi-target
# `link_one`/`unlink_one` shape: the symlink (not a copy) is deliberate —
# omp's extension loader resolves the entry's realpath before importing
# and cache-busts on mtime, so edits to this repo propagate to the next
# session with no reinstall.
#
# Idempotent; backs up any pre-existing different file; --uninstall removes
# only what this script installed.
#
# Usage:
#   plugins/kb-code/hooks/install-omp-hook.sh [--agent-dir DIR] [--check]
#   plugins/kb-code/hooks/install-omp-hook.sh --uninstall [--agent-dir DIR]
#
# Each extension shells out to its own sibling shell script via the
# resolved symlink target's directory (import.meta.dir), so both keep
# working if THIS REPO moves; re-run after moving to refresh nothing —
# only the symlink source matters, which is inside the same directory tree
# anyway.
set -eu

AGENT_DIR="${PI_CODING_AGENT_DIR:-${OMP_AGENT_DIR:-$HOME/.omp/agent}}"
HERE="$(cd "$(dirname "$0")" && pwd)"
SRC="$HERE/kb-code-omp.ts"
TARGET_NAME="kb-code.ts"
ANNOTATIONS_SRC="$HERE/kb-code-annotations-omp.ts"
ANNOTATIONS_TARGET_NAME="kb-code-annotations.ts"

usage() {
  sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'
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
ANNOTATIONS_TARGET="$EXT_DIR/$ANNOTATIONS_TARGET_NAME"

is_ours() { # <path> <src> -> 0 when <path> is our symlink to <src>
  [ -L "$1" ] && [ "$(readlink -f "$1")" = "$(readlink -f "$2")" ]
}

# link_one <src> <dir> <target> <what> — idempotent symlink install,
# backing up anything already sitting on the target path.
link_one() {
  src="$1"; dir="$2"; target="$3"; what="$4"
  if is_ours "$target" "$src"; then
    echo "omp: $what already installed — $target -> $(readlink "$target")"
    return 0
  fi
  mkdir -p "$dir"
  if [ -e "$target" ] || [ -L "$target" ]; then
    backup="$target.bak.$(date -u +%Y%m%dT%H%M%SZ)"
    mv "$target" "$backup"
    echo "omp: backed up existing $what to $backup"
  fi
  ln -s "$src" "$target"
  echo "omp: installed $what $target -> $src"
}

# unlink_one <src> <dir> <target> <what> — removes ONLY our own symlink.
unlink_one() {
  src="$1"; dir="$2"; target="$3"; what="$4"
  if [ ! -e "$target" ] && [ ! -L "$target" ]; then
    echo "omp: $target not installed — nothing to uninstall."
    return 0
  fi
  if is_ours "$target" "$src"; then
    rm -f "$target"
    rmdir "$dir" 2>/dev/null || true
    echo "omp: removed $what $target"
    return 0
  fi
  echo "omp: refusing to remove $target — not installed by this script." >&2
  return 1
}

if [ "${UNINSTALL:-0}" = 1 ]; then
  rc=0
  unlink_one "$SRC" "$EXT_DIR" "$TARGET" "provenance extension" || rc=1
  unlink_one "$ANNOTATIONS_SRC" "$EXT_DIR" "$ANNOTATIONS_TARGET" "annotations extension" || rc=1
  exit "$rc"
fi

[ -f "$SRC" ] || { echo "install-omp-hook.sh: missing $SRC" >&2; exit 1; }
[ -f "$ANNOTATIONS_SRC" ] || { echo "install-omp-hook.sh: missing $ANNOTATIONS_SRC" >&2; exit 1; }

link_one "$SRC" "$EXT_DIR" "$TARGET" "provenance extension"
link_one "$ANNOTATIONS_SRC" "$EXT_DIR" "$ANNOTATIONS_TARGET" "annotations extension"

echo "omp: loads on the NEXT session start (extensions load at startup)."
echo "omp: kb-code daemon gate: KB_CODE_DAEMON_URL=${KB_CODE_DAEMON_URL:-http://127.0.0.1:4747} (export it in your shell profile if you run kb-code on another port)."
