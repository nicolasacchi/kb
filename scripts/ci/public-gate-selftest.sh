#!/usr/bin/env bash
# Self-test for scripts/ci/public-gate.sh (run by `just ci-selfcheck`).
#
# The gate's whole value is three promises: an empty pattern is only OK when
# the run does not require one, an invalid regex is a hard failure (never "no
# hits"), and its output is file:line ONLY -- the pattern and the matched text
# must never reach the log. Each is asserted against a scratch git repo that
# carries a copy of the script (the script greps the repo it lives in).
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/scripts/ci"
cp "$here/public-gate.sh" "$work/scripts/ci/"
cd "$work"
git init -q .
git config user.email t@example.invalid
git config user.name t
printf 'harmless line\nthis has SECRETWORD42 inside\n' > leak.txt
printf 'clean\n' > clean.txt
git add -A && git commit -q -m fixture

fail=0
run() { # run <label> <want-rc> <env...> ; sets OUT
  local label="$1" want="$2"; shift 2
  OUT="$(env "$@" bash scripts/ci/public-gate.sh 2>&1)"; local rc=$?
  if [ "$rc" -ne "$want" ]; then
    echo "SELFTEST FAIL: $label: want exit $want, got $rc" >&2; echo "$OUT" >&2; fail=1
  else
    echo "ok: $label (exit $rc)"
  fi
}

run "empty pattern, not required -> skip" 0 PUBLIC_GATE_PATTERNS= PUBLIC_GATE_REQUIRED=0
case "$OUT" in *skipped*) ;; *) echo "SELFTEST FAIL: skip is not announced" >&2; fail=1 ;; esac
run "empty pattern, required -> FAIL" 1 PUBLIC_GATE_PATTERNS= PUBLIC_GATE_REQUIRED=1
run "unset pattern, required -> FAIL" 1 -u PUBLIC_GATE_PATTERNS PUBLIC_GATE_REQUIRED=1
run "invalid regex -> exit 2, nothing checked" 2 'PUBLIC_GATE_PATTERNS=SECRETWORD(' PUBLIC_GATE_REQUIRED=1
case "$OUT" in *SECRETWORD*) echo "SELFTEST FAIL: the invalid pattern was echoed" >&2; fail=1 ;; esac
run "a hit -> exit 1" 1 'PUBLIC_GATE_PATTERNS=SECRETWORD[0-9]+' PUBLIC_GATE_REQUIRED=1
case "$OUT" in *SECRETWORD*|*"this has"*) echo "SELFTEST FAIL: the pattern or matched text leaked into the output" >&2; echo "$OUT" >&2; fail=1 ;; esac
case "$OUT" in *leak.txt:2*) ;; *) echo "SELFTEST FAIL: file:line not reported" >&2; echo "$OUT" >&2; fail=1 ;; esac
run "no hit -> clean" 0 'PUBLIC_GATE_PATTERNS=NOSUCHTHING[0-9]+' PUBLIC_GATE_REQUIRED=1

# an allow-listed path is excluded
printf 'leak.txt\n' > scripts/ci/public-gate.allow
git add -A && git commit -q -m allow
run "allow-listed blob is excluded" 0 'PUBLIC_GATE_PATTERNS=SECRETWORD[0-9]+' PUBLIC_GATE_REQUIRED=1
exit "$fail"
