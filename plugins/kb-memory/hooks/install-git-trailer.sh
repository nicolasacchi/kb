#!/usr/bin/env bash
# install-git-trailer.sh — per-repo installer for the kb-memory
# Kb-Session commit trailer (W0.3).
#
# Sets `core.hooksPath` (a LOCAL, per-repo git config key — this script
# never touches global config) on each given repo to point at
# git-dispatch/, the fail-open passthrough dispatcher farm next to this
# script. `--uninstall` clears it back out. Idempotent either way, and
# refuses (rather than clobbering) a repo whose core.hooksPath already
# points somewhere else — e.g. a Husky-style JS hooks manager.
#
# Usage:
#   install-git-trailer.sh <repo-path>...
#   install-git-trailer.sh --uninstall <repo-path>...
set -u

self_dir="$(cd "$(dirname "$0")" && pwd)"
dispatch_dir="$self_dir/git-dispatch"

usage() {
  echo "usage: $(basename "$0") [--uninstall] <repo-path>..." >&2
  exit 2
}

uninstall=0
if [ "${1:-}" = "--uninstall" ]; then
  uninstall=1
  shift
fi

[ "$#" -ge 1 ] || usage

if [ "$uninstall" -eq 0 ] && [ ! -d "$dispatch_dir" ]; then
  echo "error: dispatcher dir not found: $dispatch_dir" >&2
  exit 1
fi

status=0
for repo in "$@"; do
  if [ ! -d "$repo" ]; then
    echo "skip: $repo — not a directory"
    status=1
    continue
  fi

  if ! git -C "$repo" rev-parse --git-dir >/dev/null 2>&1; then
    echo "skip: $repo — not a git repository"
    status=1
    continue
  fi

  current="$(git -C "$repo" config --local --get core.hooksPath 2>/dev/null)" || current=""

  if [ "$uninstall" -eq 1 ]; then
    if [ -z "$current" ]; then
      echo "ok (already unset): $repo"
    elif [ "$current" = "$dispatch_dir" ]; then
      git -C "$repo" config --local --unset core.hooksPath
      echo "uninstalled: $repo (core.hooksPath cleared)"
    else
      echo "skip: $repo — core.hooksPath is '$current' (not ours); leaving it alone"
      status=1
    fi
    continue
  fi

  if [ "$current" = "$dispatch_dir" ]; then
    echo "ok (already installed): $repo"
    continue
  fi

  if [ -n "$current" ]; then
    cat >&2 <<EOF
refuse: $repo already has core.hooksPath = '$current'
  A different hooks manager is likely installed here (e.g. Husky).
  Overwriting it would silently disable it. Instead, either:
    - point THAT manager's dispatch at our git-dispatch/ directory as
      its own passthrough target ($dispatch_dir), or
    - merge our prepare-commit-msg trailer logic
      ($dispatch_dir/trailer-logic.sh) into that manager's config by hand.
EOF
    status=1
    continue
  fi

  git -C "$repo" config --local core.hooksPath "$dispatch_dir"
  echo "installed: $repo -> core.hooksPath=$dispatch_dir"
done

exit "$status"
