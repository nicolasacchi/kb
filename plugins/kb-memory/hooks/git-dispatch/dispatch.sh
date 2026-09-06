#!/usr/bin/env bash
# kb-memory git-dispatch — per-repo passthrough hook shim (W0.3).
#
# Every standard git hook name in this directory is a symlink to this
# script (installed via install-git-trailer.sh, which points a repo's
# `core.hooksPath` here — see that script; NEVER touches global config).
#
# Behavior, for whichever hook name git invoked us as ($0's basename):
#   1. If we are `prepare-commit-msg`, run trailer-logic.sh (the
#      Kb-Session commit trailer). It runs as a SEPARATE process, and
#      its result is discarded with `|| true` — any error in OUR logic
#      must never abort the commit (`--no-verify` does NOT skip
#      prepare-commit-msg, so a wedged hook here would block every
#      commit in the repo; see docs/architecture-invariants.md #4-style
#      fail-open discipline elsewhere in this codebase).
#   2. THEN, regardless of hook name, chain to the repo's OWN hook at
#      "$(git rev-parse --git-common-dir)/hooks/<name>" if it exists
#      and is executable — worktree-correct (a linked worktree has no
#      private hooks dir; hooks always live in the common git dir).
#      Forward all args + stdin, and PROPAGATE ITS EXIT CODE: a real
#      pre-commit must still be able to fail the commit. Fail-open
#      applies ONLY to our own trailer logic, never to the user's hook.
set -u

hook_name="$(basename "$0")"
self_dir="$(cd "$(dirname "$0")" && pwd)"

if [ "$hook_name" = "prepare-commit-msg" ]; then
  # stdin redirected from /dev/null: prepare-commit-msg never gets
  # anything on stdin, and this guarantees trailer-logic.sh can't
  # accidentally consume bytes meant for a chained hook below.
  bash "$self_dir/trailer-logic.sh" "$@" < /dev/null || true
fi

common_dir="$(git rev-parse --git-common-dir 2>/dev/null)" || common_dir=""
if [ -n "$common_dir" ]; then
  original="$common_dir/hooks/$hook_name"
  if [ -x "$original" ]; then
    exec "$original" "$@"
  fi
fi

exit 0
