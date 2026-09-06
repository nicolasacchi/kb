#!/usr/bin/env bash
# install-omp-hooks.sh — wire kb memory into Oh My Pi (omp) by symlinking
# hooks/kb-omp.ts into the omp agent extensions directory, where omp's
# native auto-discovery loads every *.ts as an extension module, AND
# hooks/omp-agents/kb-librarian.md into the omp agents directory, where
# omp discovers every *.md as a task-agent definition. Also symlinks
# hooks/kb-omp-memory-backend.ts alongside kb-omp.ts — a second, independent
# extension that registers kb as a selectable `memory.backend: "kb"` via the
# PROPOSED `pi.registerMemoryBackend` API (a byte-silent no-op on any omp
# build that lacks it, which is every release as of writing; see that
# file's header for the double-coverage note against kb-omp.ts's own lane).
#
# The symlink (not a copy) is deliberate: omp's extension loader resolves
# the entry's realpath before importing and cache-busts on mtime, so edits
# to this repo propagate to the next session with no reinstall — the same
# operator-managed relationship ~/.config/opencode/plugin/kb-memory.ts has.
# Agent definitions are re-read per dispatch, so the same holds for them.
#
# Idempotent; backs up any pre-existing different file; --uninstall removes
# only what this script installed.
#
# Usage:
#   plugins/kb-memory/hooks/install-omp-hooks.sh [--agent-dir DIR] [--check]
#   plugins/kb-memory/hooks/install-omp-hooks.sh --uninstall [--agent-dir DIR]
#
# The extension shells out to its sibling scripts (kb-recall.sh,
# kb-wake.sh, kb-capture-omp.sh, kb-distill-nudge-omp.sh, kb-beat*.sh) via
# the resolved symlink target's directory, so it keeps working if THIS REPO
# moves; re-run after moving to refresh nothing — only the symlink source
# matters, which is inside the same directory tree anyway.
set -eu

AGENT_DIR="${PI_CODING_AGENT_DIR:-${OMP_AGENT_DIR:-$HOME/.omp/agent}}"
HERE="$(cd "$(dirname "$0")" && pwd)"
SRC="$HERE/kb-omp.ts"
TARGET_NAME="kb-memory.ts"
BACKEND_SRC="$HERE/kb-omp-memory-backend.ts"
BACKEND_TARGET_NAME="kb-memory-backend.ts"
AGENT_SRC="$HERE/omp-agents/kb-librarian.md"
AGENT_TARGET_NAME="kb-librarian.md"

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
    *) echo "install-omp-hooks.sh: unknown option: $1" >&2; usage 1 ;;
  esac
done

EXT_DIR="$AGENT_DIR/extensions"
TARGET="$EXT_DIR/$TARGET_NAME"
BACKEND_TARGET="$EXT_DIR/$BACKEND_TARGET_NAME"
AGENTS_DIR="$AGENT_DIR/agents"
AGENT_TARGET="$AGENTS_DIR/$AGENT_TARGET_NAME"

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
  unlink_one "$SRC" "$EXT_DIR" "$TARGET" "extension" || rc=1
  unlink_one "$BACKEND_SRC" "$EXT_DIR" "$BACKEND_TARGET" "memory-backend extension" || rc=1
  unlink_one "$AGENT_SRC" "$AGENTS_DIR" "$AGENT_TARGET" "task agent" || rc=1
  exit "$rc"
fi

[ -f "$SRC" ] || { echo "install-omp-hooks.sh: missing $SRC" >&2; exit 1; }
[ -f "$BACKEND_SRC" ] || { echo "install-omp-hooks.sh: missing $BACKEND_SRC" >&2; exit 1; }
[ -f "$AGENT_SRC" ] || { echo "install-omp-hooks.sh: missing $AGENT_SRC" >&2; exit 1; }

link_one "$SRC" "$EXT_DIR" "$TARGET" "extension"
link_one "$BACKEND_SRC" "$EXT_DIR" "$BACKEND_TARGET" "memory-backend extension"
link_one "$AGENT_SRC" "$AGENTS_DIR" "$AGENT_TARGET" "task agent"

echo "omp: loads on the NEXT session start (extensions load at startup)."
echo "omp: kb tools = kb_context/kb_recall/kb_remember/kb_why/kb_recollect + kb_slate_{open,delta,post} + kbcode_*; commands = /kb-context, /kb-desk, /kb-slate, /kb-distill; kill switch = --kb-off."
echo "omp: memory.backend=\"kb\" is ALSO available if the installed omp supports pi.registerMemoryBackend (silent no-op otherwise) — see kb-omp-memory-backend.ts header for the double-coverage note against the tools/injection lane above."
echo "omp: sessions corpus gate: KB_SESSIONS_DIR=${KB_SESSIONS_DIR:-$HOME/kb/sessions} (export it in your shell profile if you use another dir)."
