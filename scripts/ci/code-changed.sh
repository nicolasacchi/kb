#!/usr/bin/env bash
# The ONE path filter behind the five path-filtered `code-*` jobs of ci.yml
# (code-drift, code-lint, code-test, code-spa, code-e2e). It used to be pasted
# five times, and the five copies' "Skipped" messages drifted from the regex.
# Now the prefix list lives in exactly one place (below) and both the regex and
# the human skip message are BUILT from it.
#
# Usage:
#   BASE_SHA=... HEAD_SHA=... scripts/ci/code-changed.sh
#       decides run=true|false from `git diff --name-only BASE HEAD`, appends
#       it to $GITHUB_OUTPUT (stdout when unset). No usable base -> run=true.
#   scripts/ci/code-changed.sh --names-file FILE
#       decides from an explicit name list (the self-test uses this).
#   scripts/ci/code-changed.sh --skip-message
#       prints the "Skipped ..." sentence for the skip step.
#   scripts/ci/code-changed.sh --regex
#       prints the ERE (the self-check compares it against ci.yml).
#
# SIGPIPE: the name list is written to a FILE and matched with
# `grep -E ... FILE >/dev/null`. The old `git diff | grep -qE` form exits grep
# at the first match; on a name list bigger than the pipe buffer git then dies
# with SIGPIPE (141) and, under `set -o pipefail`, the `if` took the SKIP
# branch -- the bigger the change, the likelier every code-* job went green
# without running. Reading from a file has no writer to kill.
set -euo pipefail

# Directory-style prefixes (matched at the start of a path).
PREFIXES=(
  "crates/kb-code-server/"
  "crates/kb-code-cli/"
  "crates/kb-core/"
  "crates/kb-server/"
  "crates/kb-lip/"
  "plugins/kb-code/"
  "web-code/"
  "web/"
  "scripts/review-store/"
)
# Exact file paths.
EXACT=(
  "Cargo.toml"
  "Cargo.lock"
  "justfile"
  "rust-toolchain.toml"
  ".github/workflows/ci.yml"
  ".github/actions/setup-rust/action.yml"
  "scripts/ci/code-changed.sh"
)

esc() { printf '%s' "$1" | sed -e 's/[][\.*^$+?(){}|]/\\&/g'; }

build_regex() {
  local alts=() p
  for p in "${PREFIXES[@]}"; do alts+=("$(esc "$p")"); done
  for p in "${EXACT[@]}"; do alts+=("$(esc "$p")\$"); done
  local IFS='|'
  printf '^(%s)' "${alts[*]}"
}

skip_message() {
  local all=("${PREFIXES[@]}" "${EXACT[@]}") out="" p
  for p in "${all[@]}"; do out+="${out:+, }$p"; done
  printf 'No changes under or to: %s -- skipping.' "$out"
}

emit() {
  if [ -n "${GITHUB_OUTPUT:-}" ]; then echo "run=$1" >> "$GITHUB_OUTPUT"; else echo "run=$1"; fi
}

matches() { # matches FILE -> 0 when any line matches
  local rc=0
  grep -E "$(build_regex)" "$1" >/dev/null || rc=$?
  case "$rc" in 0) return 0 ;; 1) return 1 ;; *) echo "grep failed ($rc)" >&2; exit "$rc" ;; esac
}

case "${1:-}" in
  --regex) build_regex; echo; exit 0 ;;
  --skip-message) skip_message; echo; exit 0 ;;
  --names-file)
    if matches "${2:?--names-file needs a path}"; then emit true; else emit false; fi
    exit 0 ;;
  "") ;;
  *) echo "unknown argument: $1" >&2; exit 2 ;;
esac

if [ -z "${BASE_SHA:-}" ] || ! git cat-file -e "${BASE_SHA}^{commit}" 2>/dev/null; then
  # No usable base (first push to a branch, all-zero SHA): run rather than skip.
  emit true
  exit 0
fi
names="$(mktemp)"
trap 'rm -f "$names"' EXIT
git diff --name-only "${BASE_SHA}" "${HEAD_SHA:-HEAD}" > "$names"
if matches "$names"; then emit true; else emit false; fi
