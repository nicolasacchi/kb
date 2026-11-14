#!/usr/bin/env bash
# Self-test for scripts/ci/tmux-smoke.sh with fake tmux binaries. No real
# tmux runs here.
#
# 1. A failing fake: the JUnit must carry an XML-escaped tail of the case
#    output in <failure> (and must still parse), a passing case must have
#    no <failure>, the failure tail must reach the stderr log, and the
#    script must exit non-zero. The kill-server case passes against the
#    fake because its list-sessions fails, which is exactly its pass
#    condition.
# 2. A hanging fake (sleeps on -V): the per-command `timeout 10` must
#    bound the version case. The run is wrapped in an outer `timeout` so
#    a missing inner timeout fails this test instead of hanging CI.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SMOKE="${SCRIPT_DIR}/tmux-smoke.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

fail() { echo "TMUX-SMOKE-SELFTEST FAIL: $*" >&2; exit 1; }

# --- run 1: failing fake with XML-special output -------------------------
FAKE1="${WORK}/fake1"
cat > "${FAKE1}" <<'EOS'
#!/bin/sh
printf 'bell \a err <tag> & "quote" fake tmux error output\n' >&2
exit 1
EOS
chmod +x "${FAKE1}"

if env KB_TMUX_BIN="${FAKE1}" \
    bash "${SMOKE}" "${WORK}/fail.xml" \
    >"${WORK}/fail-stdout.log" 2>"${WORK}/fail-stderr.log"; then
  fail "run 1: smoke passed with a failing tmux"
fi
test -f "${WORK}/fail.xml" || fail "run 1: no JUnit was written"
python3 -I -c 'import sys, xml.etree.ElementTree as ET; ET.parse(sys.argv[1])' \
  "${WORK}/fail.xml" || fail "run 1: JUnit does not parse"
grep -F 'tests="5"' "${WORK}/fail.xml" >/dev/null \
  || fail "run 1: JUnit does not hold five testcases"
grep -F 'failures="4"' "${WORK}/fail.xml" >/dev/null \
  || fail "run 1: expected four failures (kill case passes)"
grep -F 'err &lt;tag&gt; &amp; &quot;quote&quot; fake tmux error output' \
  "${WORK}/fail.xml" >/dev/null \
  || fail "run 1: JUnit failure lacks the escaped output tail"
if grep -F "<tag>" "${WORK}/fail.xml" >/dev/null; then
  fail "run 1: raw markup from the case output reached the JUnit"
fi
grep -F '<testcase classname="tmux-smoke" name="kill-server-removes-socket"/>' \
  "${WORK}/fail.xml" >/dev/null \
  || fail "run 1: passing case carries a <failure>"
grep -F "fake tmux error output" "${WORK}/fail-stderr.log" >/dev/null \
  || fail "run 1: failure tail did not reach the stderr log"

# --- run 2: hanging version check is bounded -----------------------------
FAKE2="${WORK}/fake2"
cat > "${FAKE2}" <<'EOS'
#!/bin/sh
if [ "$1" = "-V" ]; then
  sleep 10000
fi
echo "fake tmux error output" >&2
exit 1
EOS
chmod +x "${FAKE2}"

if env KB_TMUX_BIN="${FAKE2}" timeout 45 \
    bash "${SMOKE}" "${WORK}/hang.xml" \
    >"${WORK}/hang-stdout.log" 2>"${WORK}/hang-stderr.log"; then
  fail "run 2: smoke passed with a failing tmux"
fi
test -f "${WORK}/hang.xml" \
  || fail "run 2: hung version check was not bounded (run did not finish)"
grep -E '<testcase classname="tmux-smoke" name="version-is-3.7c"><failure' \
  "${WORK}/hang.xml" >/dev/null \
  || fail "run 2: version case did not fail after the deadline"

say() { printf '%s\n' "$*"; }
say "tmux-smoke-selftest: failure XML, stderr tail, deadline — all pass"
