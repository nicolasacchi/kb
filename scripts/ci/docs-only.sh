#!/usr/bin/env bash
# The ONE docs-only fast path behind the compile-heavy, UNFILTERED jobs of
# ci.yml (workspace-lint, workspace-test, drift, e2e, embedder-lint,
# embedder-test, term-e2e). Sibling of scripts/ci/code-changed.sh and built the
# same way: no third-party paths-filter action, the name list is matched from a
# FILE (SIGPIPE-proof, see that script's header), and the jobs gate their STEPS
# on the verdict -- never the job -- so every job still reports and `ci-gate`
# and the required checks keep working.
#
# The verdict is `run=false` ONLY when ALL of these hold:
#   * the event is a pull_request (push to main and workflow_dispatch ALWAYS
#     run everything: main is the reference the PR caches and floors are
#     measured against, and a dispatch is somebody asking for a full gate);
#   * the diff against the base is non-empty and resolvable;
#   * EVERY changed path is on the allowlist below.
# Anything else -- an unknown file, an empty list, an unresolvable base -- is
# run=true. The allowlist is therefore the whole risk surface, and it is kept
# to paths NO job consumes. A file is on it only because a repo-wide search
# (justfile, scripts/, ci/, crates/*/tests, build.rs, web*/src, ui/, tests/,
# plugins/, Dockerfile, .dockerignore) found no reader of it:
#   docs/research/        frozen design record; scripts/check-doc-anchors.sh
#                         excludes it by design; only the nightly bench (its own
#                         workflow) indexes it
#   docs/bench/           dated bench output, never read
#   docs/guide/           the user guide (html), never read by a test
#   packaging/release-notes/  read only by release.yml, at tag time
#   SECURITY.md CLA.md CONTRIBUTING.md   no reader (dco-check.sh only NAMES the
#                         last two in an error string)
# Deliberately NOT on it, because a job or test consumes them: the top-level
# docs/*.md (doc-anchor gate, api-docs-check <- docs/api-routes.md +
# http-api.md, docs/configuration.md and docs/cli.md asserted by kb-core,
# kb-code-server and kb-cli tests, docs/public-mirror.md include_str!'d,
# architecture-invariants.md / invariant-test-map.md read by check-invariants
# and the kb-server/kb-cli test harnesses, web-ui.md by check-kt-amendments),
# README.md, CLAUDE.md (any depth), AGENTS.md, THIRD-PARTY-LICENSES.md (the
# NOTICE drift gate), plugin and skill markdown (kb-code-cli parses the skills).
# The always-on `supply-chain` job runs the doc gates themselves, so a change
# to those docs is still checked even though it also runs the compile jobs.
#
# Usage:
#   EVENT_NAME=pull_request BASE_SHA=... HEAD_SHA=... scripts/ci/docs-only.sh
#       appends run=true|false to $GITHUB_OUTPUT (stdout when unset).
#   scripts/ci/docs-only.sh --names-file FILE   verdict from a name list
#       (the self-test; ignores the event).
#   scripts/ci/docs-only.sh --skip-message      the "Skipped ..." sentence.
#   scripts/ci/docs-only.sh --regex             the ERE (self-test reads it).
set -euo pipefail

PREFIXES=(
  "docs/research/"
  "docs/bench/"
  "docs/guide/"
  "packaging/release-notes/"
)
EXACT=(
  "SECURITY.md"
  "CLA.md"
  "CONTRIBUTING.md"
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
  printf 'Documentation-only change (only under or to: %s) -- compile and test steps skipped; the job still reports.' "$out"
}

emit() {
  if [ -n "${GITHUB_OUTPUT:-}" ]; then echo "run=$1" >> "$GITHUB_OUTPUT"; else echo "run=$1"; fi
}

# verdict FILE -> prints true when anything is NOT docs-only (or the list is empty)
verdict() {
  [ -s "$1" ] || { echo true; return; }
  local rc=0
  grep -vE "$(build_regex)" "$1" >/dev/null || rc=$?
  case "$rc" in
    0) echo true ;;   # a line that is not on the allowlist
    1) echo false ;;  # every line is on it
    *) echo "grep failed ($rc)" >&2; exit "$rc" ;;
  esac
}

case "${1:-}" in
  --regex) build_regex; echo; exit 0 ;;
  --skip-message) skip_message; echo; exit 0 ;;
  --names-file) emit "$(verdict "${2:?--names-file needs a path}")"; exit 0 ;;
  "") ;;
  *) echo "unknown argument: $1" >&2; exit 2 ;;
esac

if [ "${EVENT_NAME:-}" != pull_request ]; then
  emit true  # push to main, workflow_dispatch, anything unknown: never skip
  exit 0
fi
if [ -z "${BASE_SHA:-}" ] || ! git cat-file -e "${BASE_SHA}^{commit}" 2>/dev/null; then
  emit true
  exit 0
fi
names="$(mktemp)"
trap 'rm -f "$names"' EXIT
git diff --name-only "${BASE_SHA}" "${HEAD_SHA:-HEAD}" > "$names"
emit "$(verdict "$names")"
