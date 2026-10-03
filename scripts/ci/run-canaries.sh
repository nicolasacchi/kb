#!/usr/bin/env bash
# Invariant canaries: prove each `// invariant:N` pin actually FAILS when its
# invariant is broken. A counted marker above a test is not evidence the test
# can fail (the O7 finding: deleting the /capture route_layer failed nothing).
#
# Each ci/canaries/NN-*.sh is one small ANCHORED edit that breaks one invariant,
# plus the named test(s) that must then FAIL. The edit asserts its anchor
# matched exactly once (ci/canaries/edit_once.py), otherwise the canary itself
# errors -- a canary that no longer applies cannot pass quietly.
#
# Verdicts per canary:
#   caught   the named tests FAILED (and the tree still compiled)
#   ESCAPED  the tests passed with the invariant broken -> the pin is decorative
#   BROKEN   the edit would not apply, the tree did not compile, or the named
#            test did not run -> the canary rotted; fix the canary
#
# Usage:
#   scripts/ci/run-canaries.sh                 apply + run every canary (needs cargo-nextest)
#   scripts/ci/run-canaries.sh 02              only canaries whose file name starts with / contains 02
#   scripts/ci/run-canaries.sh --check-anchors apply + revert every edit WITHOUT running tests
#                                              (compile-free; run by `just ci-selfcheck`)
# Every touched file is backed up first and restored afterwards, also on
# interrupt, so a failed run never leaves a broken tree behind.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

mode=run
filter=""
case "${1:-}" in
  --check-anchors) mode=anchors ;;
  "") ;;
  *) filter="$1" ;;
esac

backup="$(mktemp -d)"
current_files=()
restore() {
  local f
  for f in "${current_files[@]}"; do
    [ -f "$backup/$f" ] && cp "$backup/$f" "$f"
  done
  current_files=()
}
trap 'restore; rm -rf "$backup"' EXIT INT TERM

shopt -s nullglob
canaries=(ci/canaries/[0-9][0-9]-*.sh)
if [ "${#canaries[@]}" -eq 0 ]; then echo "no canaries found -- refusing to pass vacuously" >&2; exit 1; fi

caught=0; bad=0; total=0
for c in "${canaries[@]}"; do
  case "$c" in *"$filter"*) ;; *) continue ;; esac
  total=$((total + 1))
  unset CANARY_DESC CANARY_FILES CANARY_NEXTEST CANARY_MUST_FAIL
  # shellcheck disable=SC1090
  . "$c"
  echo "=== $c: $CANARY_DESC"
  for f in "${CANARY_FILES[@]}"; do
    mkdir -p "$backup/$(dirname "$f")"; cp "$f" "$backup/$f"
  done
  current_files=("${CANARY_FILES[@]}")
  if ! canary_apply; then
    echo "BROKEN   $c: the edit did not apply (anchor drifted?)"
    restore; bad=$((bad + 1)); continue
  fi
  if cmp -s "$backup/${CANARY_FILES[0]}" "${CANARY_FILES[0]}"; then
    echo "BROKEN   $c: the edit changed nothing"
    restore; bad=$((bad + 1)); continue
  fi
  if [ "$mode" = anchors ]; then
    echo "anchor ok  $c (edit applied exactly once; reverted)"
    restore; caught=$((caught + 1)); continue
  fi
  out="$(mktemp)"
  cargo nextest run --locked --profile ci --color never --no-fail-fast "${CANARY_NEXTEST[@]}" > "$out" 2>&1
  restore
  if grep -E 'could not compile' "$out" >/dev/null; then
    echo "BROKEN   $c: the mutated tree did not compile -- a compile error is not a caught invariant"
    tail -30 "$out"; bad=$((bad + 1)); rm -f "$out"; continue
  fi
  missing=()
  for t in "${CANARY_MUST_FAIL[@]}"; do
    if ! grep -E "^[[:space:]]*FAIL .*${t}" "$out" >/dev/null; then missing+=("$t"); fi
  done
  if [ "${#missing[@]}" -eq 0 ]; then
    echo "caught   $c: every named pin failed: ${CANARY_MUST_FAIL[*]}"
    caught=$((caught + 1))
  else
    if grep -E "^[[:space:]]*PASS .*(${missing[0]})" "$out" >/dev/null; then
      echo "ESCAPED  $c: with the invariant broken these pins still PASSED: ${missing[*]} (a decorative pin)"
    else
      echo "BROKEN   $c: the named test(s) did not run at all: ${missing[*]}"
    fi
    tail -25 "$out"
    bad=$((bad + 1))
  fi
  rm -f "$out"
done

if [ "$total" -eq 0 ]; then echo "filter '$filter' matched no canary" >&2; exit 1; fi
echo "canaries: $caught of $total ok ($mode)"
[ "$bad" -eq 0 ]
